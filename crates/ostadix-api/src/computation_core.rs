//! Authority-free identity spine for one versioned Ostadix computation.
//!
//! This module deliberately depends only on canonical encoding and immutable
//! resource identity.  It names content-addressed facets and the derivations
//! between them, but it cannot construct admission, placement, execution, or
//! World authority.  Higher-level frontends assemble these records from OIR,
//! project, native, and runtime artifacts.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::str::FromStr;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::canonical_cbor::{decode_bounded, encode, DecodeLimits};
use crate::resource_identity::{ArtifactId, ResourceId, WorldIdentityError};

pub const OCOMPUTATION_MANIFEST_SCHEMA_V1: &str = "ostadix.ocomputation-manifest/v1";
pub const MAX_OCOMPUTATION_MANIFEST_BYTES_V1: usize = 4 * 1024 * 1024;
pub const MAX_OCOMPUTATION_FACETS_V1: usize = 65_536;
pub const MAX_OCOMPUTATION_DERIVATIONS_V1: usize = 65_536;
pub const MAX_OCOMPUTATION_DERIVATION_INPUTS_V1: usize = 4_096;
pub const MAX_OCOMPUTATION_PARENTS_V1: usize = 4_096;

pub const OPERATION_CONTRACT_SCHEMA_V1: &str = "ostadix.operation-contract/v1";
pub const OPERATION_INTERFACE_SCHEMA_V1: &str = "ostadix.operation-interface/v1";
pub const REALIZATION_DESCRIPTOR_SCHEMA_V1: &str = "ostadix.realization-descriptor/v1";
pub const REALIZATION_SET_SCHEMA_V1: &str = "ostadix.realization-set/v1";
pub const MAX_OPERATION_SEMANTIC_RECORD_BYTES_V1: usize = 4 * 1024 * 1024;
pub const MAX_OPERATION_PORTS_V1: usize = 4_096;
pub const MAX_OPERATION_SHAPE_PARAMETERS_V1: usize = 4_096;
pub const MAX_REALIZATION_REPRESENTATIONS_PER_PORT_V1: usize = 4_096;
pub const MAX_REALIZATION_EVIDENCE_V1: usize = 4_096;
pub const MAX_REALIZATION_SET_MEMBERS_V1: usize = 65_536;

const MAX_OCOMPUTATION_DECODE_ITEMS_V1: usize = 1_000_000;
const MAX_OCOMPUTATION_DECODE_DEPTH_V1: usize = 64;
const OCOMPUTATION_REVISION_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/O-COMPUTATION-REVISION/V1\0";
const OPERATION_CONTRACT_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/OPERATION-CONTRACT/V1\0";
const OPERATION_INTERFACE_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/OPERATION-INTERFACE/V1\0";
const REALIZATION_DESCRIPTOR_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/REALIZATION-DESCRIPTOR/V1\0";
const REALIZATION_SET_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/REALIZATION-SET/V1\0";

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OComputationErrorV1 {
    #[error("invalid OComputationManifestV1: {0}")]
    Invalid(String),
    #[error("invalid {record}: {reason}")]
    InvalidSemanticRecord {
        record: &'static str,
        reason: String,
    },
    #[error("OComputation canonical encoding failed: {0}")]
    Canonical(String),
    #[error("{record} canonical encoding failed: {reason}")]
    SemanticCanonical {
        record: &'static str,
        reason: String,
    },
    #[error("OComputation JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{record} JSON is invalid: {source}")]
    SemanticJson {
        record: &'static str,
        source: serde_json::Error,
    },
    #[error(transparent)]
    Identity(#[from] WorldIdentityError),
    #[error("OComputation record is {actual} bytes; maximum is {maximum}")]
    RecordTooLarge { actual: usize, maximum: usize },
    #[error("{record} is {actual} bytes; maximum is {maximum}")]
    SemanticRecordTooLarge {
        record: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("OComputation bytes are not the canonical encoding")]
    NonCanonicalEncoding,
    #[error("{record} bytes are not the canonical encoding")]
    NonCanonicalSemanticEncoding { record: &'static str },
    #[error(
        "facet `{facet}` content mismatch: manifest names {expected}, supplied bytes hash to {actual}"
    )]
    FacetContentMismatch {
        facet: FacetIdV1,
        expected: ArtifactId,
        actual: ArtifactId,
    },
    #[error("duplicate facet identity `{facet}`")]
    DuplicateFacet { facet: FacetIdV1 },
    #[error("facet identity `{facet}` has conflicting declarations")]
    ConflictingFacet { facet: FacetIdV1 },
    #[error("derived facet `{facet}` has no derivation producer")]
    OrphanFacet { facet: FacetIdV1 },
    #[error("facet `{facet}` has multiple derivation producers")]
    MultipleProducers { facet: FacetIdV1 },
    #[error("facet derivation graph contains a cycle")]
    DerivationCycle,
}

fn invalid(reason: impl Into<String>) -> OComputationErrorV1 {
    OComputationErrorV1::Invalid(reason.into())
}

fn invalid_semantic_record(record: &'static str, reason: impl Into<String>) -> OComputationErrorV1 {
    OComputationErrorV1::InvalidSemanticRecord {
        record,
        reason: reason.into(),
    }
}

fn invalid_realization_closure_v1(reason: impl Into<String>) -> OComputationErrorV1 {
    invalid_semantic_record("operation realization closure V1", reason)
}

fn validate_realization_closure_counts_v1(
    descriptor_count: usize,
    member_count: usize,
) -> Result<(), OComputationErrorV1> {
    if descriptor_count > MAX_REALIZATION_SET_MEMBERS_V1 {
        return Err(invalid_realization_closure_v1(format!(
            "supplied descriptor count {descriptor_count} exceeds {MAX_REALIZATION_SET_MEMBERS_V1}"
        )));
    }
    if member_count > MAX_REALIZATION_SET_MEMBERS_V1 {
        return Err(invalid_realization_closure_v1(format!(
            "realization set member count {member_count} exceeds {MAX_REALIZATION_SET_MEMBERS_V1}"
        )));
    }
    if descriptor_count != member_count {
        return Err(invalid_realization_closure_v1(format!(
            "supplied descriptor count {descriptor_count} does not match realization set membership count {member_count}"
        )));
    }
    Ok(())
}

fn is_reserved_digest(digest: &ArtifactId) -> bool {
    digest.as_sha256().bytes().all(|byte| byte == b'0')
}

/// Enduring identity for a computation across immutable revisions.
///
/// The spelling is a validated relative resource path, so names such as
/// `examples/semantic-custody` and `project/compiler` remain portable while
/// absolute filesystem paths are rejected.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComputationLineageId(ResourceId);

impl ComputationLineageId {
    pub fn new(value: impl Into<String>) -> Result<Self, WorldIdentityError> {
        ResourceId::new(value).map(Self)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ComputationLineageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ComputationLineageId {
    type Err = WorldIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

/// Local identity of one facet role within a computation manifest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FacetIdV1(ResourceId);

impl FacetIdV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, WorldIdentityError> {
        ResourceId::new(value).map(Self)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for FacetIdV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for FacetIdV1 {
    type Err = WorldIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

/// Validated schema or transformer name carried by a descriptive record.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComputationTokenV1(ResourceId);

impl ComputationTokenV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, WorldIdentityError> {
        ResourceId::new(value).map(Self)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ComputationTokenV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ComputationTokenV1 {
    type Err = WorldIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

/// Exact immutable state of one computation lineage.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ComputationRevisionId(ArtifactId);

impl ComputationRevisionId {
    pub fn from_artifact(artifact: ArtifactId) -> Result<Self, OComputationErrorV1> {
        if is_reserved_digest(&artifact) {
            return Err(invalid(
                "computation revision uses the reserved all-zero digest",
            ));
        }
        Ok(Self(artifact))
    }

    pub fn artifact(&self) -> &ArtifactId {
        &self.0
    }

    pub fn as_sha256(&self) -> &str {
        self.0.as_sha256()
    }
}

impl fmt::Display for ComputationRevisionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ocomputation:sha256:{}", self.as_sha256())
    }
}

impl<'de> Deserialize<'de> for ComputationRevisionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let artifact = ArtifactId::deserialize(deserializer)?;
        Self::from_artifact(artifact).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FacetKindV1 {
    Source,
    ProjectBundle,
    ParsedDocument,
    OirProgram,
    ExecutionPlan,
    LogicalHgraph,
    SolvedHgraph,
    HgraphRendering,
    ScheduleExplanation,
    ExecutionIntent,
    TransferVocabulary,
    Evidence,
    AdmissionRecord,
    Deployment,
    Placement,
    NativePackage,
    RuntimeJournal,
    RuntimeGraph,
    TerminalObservation,
    Receipt,
    InformationProjection,
    OperationContract,
    OperationInterface,
    RealizationDescriptor,
    RealizationSet,
    PhysicalRepresentation,
    TransferPlan,
    CostProfile,
    Objective,
    RecoveryPlan,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FacetRefV1 {
    pub id: FacetIdV1,
    pub kind: FacetKindV1,
    pub schema: ComputationTokenV1,
    pub content: ArtifactId,
}

impl FacetRefV1 {
    pub fn new(
        id: FacetIdV1,
        kind: FacetKindV1,
        schema: ComputationTokenV1,
        content: ArtifactId,
    ) -> Self {
        Self {
            id,
            kind,
            schema,
            content,
        }
    }
}

macro_rules! semantic_resource_id_v1 {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(ResourceId);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, WorldIdentityError> {
                ResourceId::new(value).map(Self)
            }

            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = WorldIdentityError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
    };
}

semantic_resource_id_v1!(
    OperationIdV1,
    "Stable semantic name of an operation, independent of any implementation."
);
semantic_resource_id_v1!(
    RealizationIdV1,
    "Stable semantic name of one declared realization."
);

macro_rules! semantic_digest_id_v1 {
    ($name:ident, $label:literal, $display_prefix:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(ArtifactId);

        impl $name {
            pub fn from_artifact(artifact: ArtifactId) -> Result<Self, OComputationErrorV1> {
                if is_reserved_digest(&artifact) {
                    return Err(invalid_semantic_record(
                        stringify!($name),
                        concat!($label, " uses the reserved all-zero digest"),
                    ));
                }
                Ok(Self(artifact))
            }

            pub fn from_sha256(value: impl Into<String>) -> Result<Self, OComputationErrorV1> {
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
                write!(formatter, concat!($display_prefix, "{}"), self.as_sha256())
            }
        }

        impl FromStr for $name {
            type Err = OComputationErrorV1;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::from_sha256(value.strip_prefix($display_prefix).unwrap_or(value))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let artifact = ArtifactId::deserialize(deserializer)?;
                Self::from_artifact(artifact).map_err(serde::de::Error::custom)
            }
        }
    };
}

semantic_digest_id_v1!(
    OperationContractIdV1,
    "operation contract identity",
    "operation-contract:sha256:"
);
semantic_digest_id_v1!(
    OperationInterfaceIdV1,
    "operation interface identity",
    "operation-interface:sha256:"
);
semantic_digest_id_v1!(
    RealizationDescriptorIdV1,
    "realization descriptor identity",
    "realization-descriptor:sha256:"
);
semantic_digest_id_v1!(
    RealizationSetIdV1,
    "realization set identity",
    "realization-set:sha256:"
);

/// A typed immutable semantic document reference.
///
/// The pair is descriptive only: it contains neither a locator nor permission
/// to retrieve, invoke, admit, or execute the named content.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticArtifactRefV1 {
    pub schema: ComputationTokenV1,
    pub content: ArtifactId,
}

impl SemanticArtifactRefV1 {
    pub fn new(
        schema: ComputationTokenV1,
        content: ArtifactId,
    ) -> Result<Self, OComputationErrorV1> {
        let reference = Self { schema, content };
        reference.validate("semantic artifact reference")?;
        Ok(reference)
    }

    fn validate(&self, context: &str) -> Result<(), OComputationErrorV1> {
        if is_reserved_digest(&self.content) {
            return Err(invalid_semantic_record(
                "SemanticArtifactRefV1",
                format!("{context} uses the reserved all-zero content digest"),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationPortV1 {
    pub name: ComputationTokenV1,
    pub value_type: SemanticArtifactRefV1,
}

impl OperationPortV1 {
    pub fn new(
        name: ComputationTokenV1,
        value_type: SemanticArtifactRefV1,
    ) -> Result<Self, OComputationErrorV1> {
        value_type.validate("operation port value type")?;
        Ok(Self { name, value_type })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationShapeParameterV1 {
    pub name: ComputationTokenV1,
    pub constraint: SemanticArtifactRefV1,
}

impl OperationShapeParameterV1 {
    pub fn new(
        name: ComputationTokenV1,
        constraint: SemanticArtifactRefV1,
    ) -> Result<Self, OComputationErrorV1> {
        constraint.validate("operation shape constraint")?;
        Ok(Self { name, constraint })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealizationPortRepresentationsV1 {
    pub port: ComputationTokenV1,
    pub representations: Vec<SemanticArtifactRefV1>,
}

impl RealizationPortRepresentationsV1 {
    pub fn new(
        port: ComputationTokenV1,
        representations: Vec<SemanticArtifactRefV1>,
    ) -> Result<Self, OComputationErrorV1> {
        let mut binding = Self {
            port,
            representations,
        };
        binding.validate("realization port representations")?;
        binding.representations.sort();
        binding.validate("realization port representations")?;
        Ok(binding)
    }

    fn validate(&self, context: &str) -> Result<(), OComputationErrorV1> {
        if self.representations.is_empty() {
            return Err(invalid_semantic_record(
                "RealizationPortRepresentationsV1",
                format!("{context} is empty for `{}`", self.port),
            ));
        }
        if self.representations.len() > MAX_REALIZATION_REPRESENTATIONS_PER_PORT_V1 {
            return Err(invalid_semantic_record(
                "RealizationPortRepresentationsV1",
                format!(
                    "{context} count {} exceeds {MAX_REALIZATION_REPRESENTATIONS_PER_PORT_V1} for `{}`",
                    self.representations.len(),
                    self.port
                ),
            ));
        }
        let mut unique = BTreeSet::new();
        for representation in &self.representations {
            representation.validate(context)?;
            if !unique.insert(representation) {
                return Err(invalid_semantic_record(
                    "RealizationPortRepresentationsV1",
                    format!("{context} repeats a representation for `{}`", self.port),
                ));
            }
        }
        Ok(())
    }
}

trait CanonicalSemanticRecordV1: Clone + Serialize + DeserializeOwned {
    const RECORD_FAMILY: &'static str;

    fn canonicalize(&mut self) {}

    fn validate(&self) -> Result<(), OComputationErrorV1>;
}

fn verify_semantic_record_v1<T>(mut record: T) -> Result<T, OComputationErrorV1>
where
    T: CanonicalSemanticRecordV1,
{
    record.validate()?;
    record.canonicalize();
    record.validate()?;
    let _ = encode_semantic_record_v1(&record)?;
    Ok(record)
}

fn encode_semantic_record_v1<T>(record: &T) -> Result<Vec<u8>, OComputationErrorV1>
where
    T: CanonicalSemanticRecordV1,
{
    let bytes = encode(record).map_err(|error| OComputationErrorV1::SemanticCanonical {
        record: T::RECORD_FAMILY,
        reason: error.to_string(),
    })?;
    if bytes.len() > MAX_OPERATION_SEMANTIC_RECORD_BYTES_V1 {
        return Err(OComputationErrorV1::SemanticRecordTooLarge {
            record: T::RECORD_FAMILY,
            actual: bytes.len(),
            maximum: MAX_OPERATION_SEMANTIC_RECORD_BYTES_V1,
        });
    }
    Ok(bytes)
}

fn canonical_semantic_bytes_v1<T>(record: &T) -> Result<Vec<u8>, OComputationErrorV1>
where
    T: CanonicalSemanticRecordV1,
{
    encode_semantic_record_v1(&verify_semantic_record_v1(record.clone())?)
}

fn decode_canonical_semantic_record_v1<T>(bytes: &[u8]) -> Result<T, OComputationErrorV1>
where
    T: CanonicalSemanticRecordV1,
{
    if bytes.len() > MAX_OPERATION_SEMANTIC_RECORD_BYTES_V1 {
        return Err(OComputationErrorV1::SemanticRecordTooLarge {
            record: T::RECORD_FAMILY,
            actual: bytes.len(),
            maximum: MAX_OPERATION_SEMANTIC_RECORD_BYTES_V1,
        });
    }
    let record: T = decode_bounded(
        bytes,
        DecodeLimits {
            max_bytes: MAX_OPERATION_SEMANTIC_RECORD_BYTES_V1,
            max_items: MAX_OCOMPUTATION_DECODE_ITEMS_V1,
            max_depth: MAX_OCOMPUTATION_DECODE_DEPTH_V1,
        },
    )
    .map_err(|error| OComputationErrorV1::SemanticCanonical {
        record: T::RECORD_FAMILY,
        reason: error.to_string(),
    })?;
    let record = verify_semantic_record_v1(record)?;
    if encode_semantic_record_v1(&record)? != bytes {
        return Err(OComputationErrorV1::NonCanonicalSemanticEncoding {
            record: T::RECORD_FAMILY,
        });
    }
    Ok(record)
}

fn decode_json_semantic_record_v1<T>(bytes: &[u8]) -> Result<T, OComputationErrorV1>
where
    T: CanonicalSemanticRecordV1,
{
    if bytes.len() > MAX_OPERATION_SEMANTIC_RECORD_BYTES_V1 {
        return Err(OComputationErrorV1::SemanticRecordTooLarge {
            record: T::RECORD_FAMILY,
            actual: bytes.len(),
            maximum: MAX_OPERATION_SEMANTIC_RECORD_BYTES_V1,
        });
    }
    let record =
        serde_json::from_slice(bytes).map_err(|source| OComputationErrorV1::SemanticJson {
            record: T::RECORD_FAMILY,
            source,
        })?;
    verify_semantic_record_v1(record)
}

fn canonical_semantic_json_v1<T>(record: &T, pretty: bool) -> Result<Vec<u8>, OComputationErrorV1>
where
    T: CanonicalSemanticRecordV1,
{
    let record = verify_semantic_record_v1(record.clone())?;
    let mut bytes = if pretty {
        serde_json::to_vec_pretty(&record)
    } else {
        serde_json::to_vec(&record)
    }
    .map_err(|source| OComputationErrorV1::SemanticJson {
        record: T::RECORD_FAMILY,
        source,
    })?;
    if pretty {
        bytes.push(b'\n');
    }
    if bytes.len() > MAX_OPERATION_SEMANTIC_RECORD_BYTES_V1 {
        return Err(OComputationErrorV1::SemanticRecordTooLarge {
            record: T::RECORD_FAMILY,
            actual: bytes.len(),
            maximum: MAX_OPERATION_SEMANTIC_RECORD_BYTES_V1,
        });
    }
    Ok(bytes)
}

fn semantic_record_artifact_id_v1<T>(
    record: &T,
    domain: &[u8],
) -> Result<ArtifactId, OComputationErrorV1>
where
    T: CanonicalSemanticRecordV1,
{
    let canonical = canonical_semantic_bytes_v1(record)?;
    let length = u64::try_from(canonical.len()).map_err(|_| {
        invalid_semantic_record(
            T::RECORD_FAMILY,
            "canonical semantic record length does not fit u64",
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(length.to_be_bytes());
    hasher.update(canonical);
    Ok(ArtifactId::from_sha256(hex::encode(hasher.finalize()))?)
}

fn semantic_record_facet_ref_v1<T>(
    record: &T,
    id: FacetIdV1,
    kind: FacetKindV1,
    schema: &'static str,
) -> Result<FacetRefV1, OComputationErrorV1>
where
    T: CanonicalSemanticRecordV1,
{
    let canonical = canonical_semantic_bytes_v1(record)?;
    Ok(FacetRefV1::new(
        id,
        kind,
        ComputationTokenV1::new(schema)?,
        artifact_id_for_bytes(&canonical),
    ))
}

/// Behavioral meaning required of every realization of one operation version.
///
/// Every semantic dimension is an explicit typed artifact reference, including
/// a reference to an explicit no-op/true document when a dimension is empty.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationContractV1 {
    pub schema: String,
    pub operation: OperationIdV1,
    pub semantic_version: u64,
    pub preconditions: SemanticArtifactRefV1,
    pub postconditions: SemanticArtifactRefV1,
    pub state_model: SemanticArtifactRefV1,
    pub effect_model: SemanticArtifactRefV1,
    pub ordering: SemanticArtifactRefV1,
    pub determinism: SemanticArtifactRefV1,
    pub required_fidelity: SemanticArtifactRefV1,
}

impl OperationContractV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operation: OperationIdV1,
        semantic_version: u64,
        preconditions: SemanticArtifactRefV1,
        postconditions: SemanticArtifactRefV1,
        state_model: SemanticArtifactRefV1,
        effect_model: SemanticArtifactRefV1,
        ordering: SemanticArtifactRefV1,
        determinism: SemanticArtifactRefV1,
        required_fidelity: SemanticArtifactRefV1,
    ) -> Result<Self, OComputationErrorV1> {
        Self {
            schema: OPERATION_CONTRACT_SCHEMA_V1.to_string(),
            operation,
            semantic_version,
            preconditions,
            postconditions,
            state_model,
            effect_model,
            ordering,
            determinism,
            required_fidelity,
        }
        .verify()
    }

    pub fn verify(self) -> Result<Self, OComputationErrorV1> {
        verify_semantic_record_v1(self)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, OComputationErrorV1> {
        decode_canonical_semantic_record_v1(bytes)
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self, OComputationErrorV1> {
        decode_json_semantic_record_v1(bytes)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        canonical_semantic_bytes_v1(self)
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        canonical_semantic_json_v1(self, false)
    }

    pub fn canonical_json_pretty(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        canonical_semantic_json_v1(self, true)
    }

    pub fn id(&self) -> Result<OperationContractIdV1, OComputationErrorV1> {
        OperationContractIdV1::from_artifact(semantic_record_artifact_id_v1(
            self,
            OPERATION_CONTRACT_DIGEST_DOMAIN_V1,
        )?)
    }

    pub fn facet_ref(&self, id: FacetIdV1) -> Result<FacetRefV1, OComputationErrorV1> {
        semantic_record_facet_ref_v1(
            self,
            id,
            FacetKindV1::OperationContract,
            OPERATION_CONTRACT_SCHEMA_V1,
        )
    }
}

impl CanonicalSemanticRecordV1 for OperationContractV1 {
    const RECORD_FAMILY: &'static str = "OperationContractV1";

    fn validate(&self) -> Result<(), OComputationErrorV1> {
        if self.schema != OPERATION_CONTRACT_SCHEMA_V1 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                format!(
                    "unsupported operation contract schema `{}`; expected `{OPERATION_CONTRACT_SCHEMA_V1}`",
                    self.schema
                ),
            ));
        }
        if self.semantic_version == 0 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                "operation contract semantic_version must be positive",
            ));
        }
        for (name, reference) in [
            ("preconditions", &self.preconditions),
            ("postconditions", &self.postconditions),
            ("state_model", &self.state_model),
            ("effect_model", &self.effect_model),
            ("ordering", &self.ordering),
            ("determinism", &self.determinism),
            ("required_fidelity", &self.required_fidelity),
        ] {
            reference.validate(name)?;
        }
        Ok(())
    }
}

/// Named semantic ports and shape parameters exposed by one operation version.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationInterfaceV1 {
    pub schema: String,
    pub operation: OperationIdV1,
    pub semantic_version: u64,
    pub contract: OperationContractIdV1,
    pub shape_parameters: Vec<OperationShapeParameterV1>,
    pub inputs: Vec<OperationPortV1>,
    pub outputs: Vec<OperationPortV1>,
}

impl OperationInterfaceV1 {
    pub fn new(
        operation: OperationIdV1,
        semantic_version: u64,
        contract: OperationContractIdV1,
        shape_parameters: Vec<OperationShapeParameterV1>,
        inputs: Vec<OperationPortV1>,
        outputs: Vec<OperationPortV1>,
    ) -> Result<Self, OComputationErrorV1> {
        Self {
            schema: OPERATION_INTERFACE_SCHEMA_V1.to_string(),
            operation,
            semantic_version,
            contract,
            shape_parameters,
            inputs,
            outputs,
        }
        .verify()
    }

    pub fn verify(self) -> Result<Self, OComputationErrorV1> {
        verify_semantic_record_v1(self)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, OComputationErrorV1> {
        decode_canonical_semantic_record_v1(bytes)
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self, OComputationErrorV1> {
        decode_json_semantic_record_v1(bytes)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        canonical_semantic_bytes_v1(self)
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        canonical_semantic_json_v1(self, false)
    }

    pub fn canonical_json_pretty(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        canonical_semantic_json_v1(self, true)
    }

    pub fn id(&self) -> Result<OperationInterfaceIdV1, OComputationErrorV1> {
        OperationInterfaceIdV1::from_artifact(semantic_record_artifact_id_v1(
            self,
            OPERATION_INTERFACE_DIGEST_DOMAIN_V1,
        )?)
    }

    pub fn facet_ref(&self, id: FacetIdV1) -> Result<FacetRefV1, OComputationErrorV1> {
        semantic_record_facet_ref_v1(
            self,
            id,
            FacetKindV1::OperationInterface,
            OPERATION_INTERFACE_SCHEMA_V1,
        )
    }
}

impl CanonicalSemanticRecordV1 for OperationInterfaceV1 {
    const RECORD_FAMILY: &'static str = "OperationInterfaceV1";

    fn canonicalize(&mut self) {
        self.shape_parameters.sort();
        self.inputs.sort();
        self.outputs.sort();
    }

    fn validate(&self) -> Result<(), OComputationErrorV1> {
        if self.schema != OPERATION_INTERFACE_SCHEMA_V1 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                format!(
                    "unsupported operation interface schema `{}`; expected `{OPERATION_INTERFACE_SCHEMA_V1}`",
                    self.schema
                ),
            ));
        }
        if self.semantic_version == 0 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                "operation interface semantic_version must be positive",
            ));
        }
        if is_reserved_digest(self.contract.artifact()) {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                "operation interface uses the reserved all-zero contract digest",
            ));
        }
        if self.shape_parameters.len() > MAX_OPERATION_SHAPE_PARAMETERS_V1 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                format!(
                    "operation shape parameter count {} exceeds {MAX_OPERATION_SHAPE_PARAMETERS_V1}",
                    self.shape_parameters.len()
                ),
            ));
        }
        if self.inputs.len() > MAX_OPERATION_PORTS_V1 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                format!(
                    "operation input port count {} exceeds {MAX_OPERATION_PORTS_V1}",
                    self.inputs.len()
                ),
            ));
        }
        if self.outputs.len() > MAX_OPERATION_PORTS_V1 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                format!(
                    "operation output port count {} exceeds {MAX_OPERATION_PORTS_V1}",
                    self.outputs.len()
                ),
            ));
        }

        let mut shape_names = BTreeSet::new();
        for parameter in &self.shape_parameters {
            parameter
                .constraint
                .validate("operation shape constraint")?;
            if !shape_names.insert(&parameter.name) {
                return Err(invalid_semantic_record(
                    Self::RECORD_FAMILY,
                    format!("duplicate operation shape parameter `{}`", parameter.name),
                ));
            }
        }
        for (direction, ports) in [("input", &self.inputs), ("output", &self.outputs)] {
            let mut names = BTreeSet::new();
            for port in ports {
                port.value_type
                    .validate(&format!("operation {direction} port value type"))?;
                if !names.insert(&port.name) {
                    return Err(invalid_semantic_record(
                        Self::RECORD_FAMILY,
                        format!("duplicate operation {direction} port `{}`", port.name),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// One immutable declaration of how an operation interface may be realized.
///
/// This record can describe requirements and evidence, but cannot select a
/// winner, grant authority, inspect a target, or invoke its implementation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealizationDescriptorV1 {
    pub schema: String,
    pub realization: RealizationIdV1,
    pub interface: OperationInterfaceIdV1,
    pub contract: OperationContractIdV1,
    pub implementation: ArtifactId,
    pub execution_pipeline: SemanticArtifactRefV1,
    pub input_representations: Vec<RealizationPortRepresentationsV1>,
    pub output_representations: Vec<RealizationPortRepresentationsV1>,
    pub target_requirements: SemanticArtifactRefV1,
    pub state_requirements: SemanticArtifactRefV1,
    pub actor_requirements: SemanticArtifactRefV1,
    pub supplied_fidelity: SemanticArtifactRefV1,
    pub cost_model: Option<SemanticArtifactRefV1>,
    pub validation_evidence: Vec<SemanticArtifactRefV1>,
}

impl RealizationDescriptorV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        realization: RealizationIdV1,
        interface: OperationInterfaceIdV1,
        contract: OperationContractIdV1,
        implementation: ArtifactId,
        execution_pipeline: SemanticArtifactRefV1,
        input_representations: Vec<RealizationPortRepresentationsV1>,
        output_representations: Vec<RealizationPortRepresentationsV1>,
        target_requirements: SemanticArtifactRefV1,
        state_requirements: SemanticArtifactRefV1,
        actor_requirements: SemanticArtifactRefV1,
        supplied_fidelity: SemanticArtifactRefV1,
        cost_model: Option<SemanticArtifactRefV1>,
        validation_evidence: Vec<SemanticArtifactRefV1>,
    ) -> Result<Self, OComputationErrorV1> {
        Self {
            schema: REALIZATION_DESCRIPTOR_SCHEMA_V1.to_string(),
            realization,
            interface,
            contract,
            implementation,
            execution_pipeline,
            input_representations,
            output_representations,
            target_requirements,
            state_requirements,
            actor_requirements,
            supplied_fidelity,
            cost_model,
            validation_evidence,
        }
        .verify()
    }

    pub fn verify(self) -> Result<Self, OComputationErrorV1> {
        verify_semantic_record_v1(self)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, OComputationErrorV1> {
        decode_canonical_semantic_record_v1(bytes)
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self, OComputationErrorV1> {
        decode_json_semantic_record_v1(bytes)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        canonical_semantic_bytes_v1(self)
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        canonical_semantic_json_v1(self, false)
    }

    pub fn canonical_json_pretty(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        canonical_semantic_json_v1(self, true)
    }

    pub fn id(&self) -> Result<RealizationDescriptorIdV1, OComputationErrorV1> {
        RealizationDescriptorIdV1::from_artifact(semantic_record_artifact_id_v1(
            self,
            REALIZATION_DESCRIPTOR_DIGEST_DOMAIN_V1,
        )?)
    }

    pub fn facet_ref(&self, id: FacetIdV1) -> Result<FacetRefV1, OComputationErrorV1> {
        semantic_record_facet_ref_v1(
            self,
            id,
            FacetKindV1::RealizationDescriptor,
            REALIZATION_DESCRIPTOR_SCHEMA_V1,
        )
    }
}

impl CanonicalSemanticRecordV1 for RealizationDescriptorV1 {
    const RECORD_FAMILY: &'static str = "RealizationDescriptorV1";

    fn canonicalize(&mut self) {
        for binding in self
            .input_representations
            .iter_mut()
            .chain(self.output_representations.iter_mut())
        {
            binding.representations.sort();
        }
        self.input_representations.sort();
        self.output_representations.sort();
        self.validation_evidence.sort();
    }

    fn validate(&self) -> Result<(), OComputationErrorV1> {
        if self.schema != REALIZATION_DESCRIPTOR_SCHEMA_V1 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                format!(
                    "unsupported realization descriptor schema `{}`; expected `{REALIZATION_DESCRIPTOR_SCHEMA_V1}`",
                    self.schema
                ),
            ));
        }
        if is_reserved_digest(self.interface.artifact()) {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                "realization descriptor uses the reserved all-zero interface digest",
            ));
        }
        if is_reserved_digest(self.contract.artifact()) {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                "realization descriptor uses the reserved all-zero contract digest",
            ));
        }
        if is_reserved_digest(&self.implementation) {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                "realization descriptor uses the reserved all-zero implementation digest",
            ));
        }
        if self.input_representations.len() > MAX_OPERATION_PORTS_V1 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                format!(
                    "realization input binding count {} exceeds {MAX_OPERATION_PORTS_V1}",
                    self.input_representations.len()
                ),
            ));
        }
        if self.output_representations.len() > MAX_OPERATION_PORTS_V1 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                format!(
                    "realization output binding count {} exceeds {MAX_OPERATION_PORTS_V1}",
                    self.output_representations.len()
                ),
            ));
        }
        if self.validation_evidence.len() > MAX_REALIZATION_EVIDENCE_V1 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                format!(
                    "realization validation evidence count {} exceeds {MAX_REALIZATION_EVIDENCE_V1}",
                    self.validation_evidence.len()
                ),
            ));
        }

        for (name, reference) in [
            ("execution_pipeline", &self.execution_pipeline),
            ("target_requirements", &self.target_requirements),
            ("state_requirements", &self.state_requirements),
            ("actor_requirements", &self.actor_requirements),
            ("supplied_fidelity", &self.supplied_fidelity),
        ] {
            reference.validate(name)?;
        }
        if let Some(cost_model) = &self.cost_model {
            cost_model.validate("cost_model")?;
        }

        for (direction, bindings) in [
            ("input", &self.input_representations),
            ("output", &self.output_representations),
        ] {
            let mut ports = BTreeSet::new();
            for binding in bindings {
                binding.validate(&format!("realization {direction} representations"))?;
                if !ports.insert(&binding.port) {
                    return Err(invalid_semantic_record(
                        Self::RECORD_FAMILY,
                        format!(
                            "duplicate realization {direction} port binding `{}`",
                            binding.port
                        ),
                    ));
                }
            }
        }

        let mut evidence = BTreeSet::new();
        for item in &self.validation_evidence {
            item.validate("validation_evidence")?;
            if !evidence.insert(item) {
                return Err(invalid_semantic_record(
                    Self::RECORD_FAMILY,
                    "duplicate realization validation evidence",
                ));
            }
        }
        Ok(())
    }
}

/// Canonical membership record for the known realizations of one interface.
///
/// Membership conveys no priority, preference, eligibility, or selected winner.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealizationSetV1 {
    pub schema: String,
    pub interface: OperationInterfaceIdV1,
    pub contract: OperationContractIdV1,
    pub realizations: Vec<RealizationDescriptorIdV1>,
}

impl RealizationSetV1 {
    pub fn new(
        interface: OperationInterfaceIdV1,
        contract: OperationContractIdV1,
        realizations: Vec<RealizationDescriptorIdV1>,
    ) -> Result<Self, OComputationErrorV1> {
        Self {
            schema: REALIZATION_SET_SCHEMA_V1.to_string(),
            interface,
            contract,
            realizations,
        }
        .verify()
    }

    pub fn verify(self) -> Result<Self, OComputationErrorV1> {
        verify_semantic_record_v1(self)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, OComputationErrorV1> {
        decode_canonical_semantic_record_v1(bytes)
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self, OComputationErrorV1> {
        decode_json_semantic_record_v1(bytes)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        canonical_semantic_bytes_v1(self)
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        canonical_semantic_json_v1(self, false)
    }

    pub fn canonical_json_pretty(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        canonical_semantic_json_v1(self, true)
    }

    pub fn id(&self) -> Result<RealizationSetIdV1, OComputationErrorV1> {
        RealizationSetIdV1::from_artifact(semantic_record_artifact_id_v1(
            self,
            REALIZATION_SET_DIGEST_DOMAIN_V1,
        )?)
    }

    pub fn facet_ref(&self, id: FacetIdV1) -> Result<FacetRefV1, OComputationErrorV1> {
        semantic_record_facet_ref_v1(
            self,
            id,
            FacetKindV1::RealizationSet,
            REALIZATION_SET_SCHEMA_V1,
        )
    }
}

impl CanonicalSemanticRecordV1 for RealizationSetV1 {
    const RECORD_FAMILY: &'static str = "RealizationSetV1";

    fn canonicalize(&mut self) {
        self.realizations.sort();
    }

    fn validate(&self) -> Result<(), OComputationErrorV1> {
        if self.schema != REALIZATION_SET_SCHEMA_V1 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                format!(
                    "unsupported realization set schema `{}`; expected `{REALIZATION_SET_SCHEMA_V1}`",
                    self.schema
                ),
            ));
        }
        if is_reserved_digest(self.interface.artifact()) {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                "realization set uses the reserved all-zero interface digest",
            ));
        }
        if is_reserved_digest(self.contract.artifact()) {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                "realization set uses the reserved all-zero contract digest",
            ));
        }
        if self.realizations.is_empty() {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                "realization set contains no descriptors",
            ));
        }
        if self.realizations.len() > MAX_REALIZATION_SET_MEMBERS_V1 {
            return Err(invalid_semantic_record(
                Self::RECORD_FAMILY,
                format!(
                    "realization set member count {} exceeds {MAX_REALIZATION_SET_MEMBERS_V1}",
                    self.realizations.len()
                ),
            ));
        }
        let mut unique = BTreeSet::new();
        for realization in &self.realizations {
            if is_reserved_digest(realization.artifact()) {
                return Err(invalid_semantic_record(
                    Self::RECORD_FAMILY,
                    "realization set uses the reserved all-zero descriptor digest",
                ));
            }
            if !unique.insert(realization) {
                return Err(invalid_semantic_record(
                    Self::RECORD_FAMILY,
                    format!("realization set repeats descriptor `{realization}`"),
                ));
            }
        }
        Ok(())
    }
}

/// Verify the complete descriptive relationship between a contract, interface,
/// its supplied descriptors, and one realization set.
///
/// This proves canonical referential consistency only. It does not establish
/// behavioral equivalence, evidence authenticity, target eligibility, runtime
/// availability, proof validity, optimality, authority, or execution.
pub fn verify_realization_set_v1(
    contract: &OperationContractV1,
    interface: &OperationInterfaceV1,
    descriptors: &[RealizationDescriptorV1],
    realization_set: &RealizationSetV1,
) -> Result<(), OComputationErrorV1> {
    validate_realization_closure_counts_v1(descriptors.len(), realization_set.realizations.len())?;

    let contract = contract.clone().verify()?;
    let interface = interface.clone().verify()?;
    let realization_set = realization_set.clone().verify()?;
    let contract_id = contract.id()?;
    let interface_id = interface.id()?;

    if interface.operation != contract.operation {
        return Err(invalid_realization_closure_v1(format!(
            "operation interface names `{}` but contract names `{}`",
            interface.operation, contract.operation
        )));
    }
    if interface.semantic_version != contract.semantic_version {
        return Err(invalid_realization_closure_v1(format!(
            "operation interface semantic_version {} does not match contract semantic_version {}",
            interface.semantic_version, contract.semantic_version
        )));
    }
    if interface.contract != contract_id {
        return Err(invalid_realization_closure_v1(format!(
            "operation interface contract `{}` does not match supplied contract `{contract_id}`",
            interface.contract
        )));
    }
    if realization_set.interface != interface_id {
        return Err(invalid_realization_closure_v1(format!(
            "realization set interface `{}` does not match supplied interface `{interface_id}`",
            realization_set.interface
        )));
    }
    if realization_set.contract != contract_id {
        return Err(invalid_realization_closure_v1(format!(
            "realization set contract `{}` does not match supplied contract `{contract_id}`",
            realization_set.contract
        )));
    }

    let expected_inputs = interface
        .inputs
        .iter()
        .map(|port| &port.name)
        .collect::<BTreeSet<_>>();
    let expected_outputs = interface
        .outputs
        .iter()
        .map(|port| &port.name)
        .collect::<BTreeSet<_>>();
    let mut descriptor_ids = Vec::with_capacity(descriptors.len());
    let mut stable_realizations = BTreeSet::new();
    for descriptor in descriptors {
        let descriptor = descriptor.clone().verify()?;
        if descriptor.interface != interface_id {
            return Err(invalid_realization_closure_v1(format!(
                "realization `{}` interface `{}` does not match supplied interface `{interface_id}`",
                descriptor.realization, descriptor.interface
            )));
        }
        if descriptor.contract != contract_id {
            return Err(invalid_realization_closure_v1(format!(
                "realization `{}` contract `{}` does not match supplied contract `{contract_id}`",
                descriptor.realization, descriptor.contract
            )));
        }
        if !stable_realizations.insert(descriptor.realization.clone()) {
            return Err(invalid_realization_closure_v1(format!(
                "duplicate stable realization name `{}`",
                descriptor.realization
            )));
        }
        let actual_inputs = descriptor
            .input_representations
            .iter()
            .map(|binding| &binding.port)
            .collect::<BTreeSet<_>>();
        if actual_inputs != expected_inputs {
            return Err(invalid_realization_closure_v1(format!(
                "realization `{}` input bindings do not exactly cover the operation interface",
                descriptor.realization
            )));
        }
        let actual_outputs = descriptor
            .output_representations
            .iter()
            .map(|binding| &binding.port)
            .collect::<BTreeSet<_>>();
        if actual_outputs != expected_outputs {
            return Err(invalid_realization_closure_v1(format!(
                "realization `{}` output bindings do not exactly cover the operation interface",
                descriptor.realization
            )));
        }
        descriptor_ids.push(descriptor.id()?);
    }
    descriptor_ids.sort();
    if descriptor_ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(invalid_realization_closure_v1(
            "supplied realization descriptors contain duplicate identities",
        ));
    }
    if descriptor_ids != realization_set.realizations {
        return Err(invalid_realization_closure_v1(
            "realization set membership does not exactly match the supplied descriptors",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivationRelationV1 {
    ParsedFrom,
    LoweredFrom,
    PlannedFrom,
    ProjectedFrom,
    SolvedFrom,
    AnalyzedFrom,
    AdmittedFrom,
    PlacedFrom,
    RealizedBy,
    ObservedFrom,
    SettledFrom,
    CommittedFrom,
    ProjectedAsInformation,
}

impl DerivationRelationV1 {
    const fn canonical_name(self) -> &'static str {
        match self {
            Self::ParsedFrom => "parsed_from",
            Self::LoweredFrom => "lowered_from",
            Self::PlannedFrom => "planned_from",
            Self::ProjectedFrom => "projected_from",
            Self::SolvedFrom => "solved_from",
            Self::AnalyzedFrom => "analyzed_from",
            Self::AdmittedFrom => "admitted_from",
            Self::PlacedFrom => "placed_from",
            Self::RealizedBy => "realized_by",
            Self::ObservedFrom => "observed_from",
            Self::SettledFrom => "settled_from",
            Self::CommittedFrom => "committed_from",
            Self::ProjectedAsInformation => "projected_as_information",
        }
    }
}

/// Descriptive identity of the exact transformer specification or binary.
/// This is an immutable artifact reference, not permission to invoke it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformIdentityV1 {
    pub name: ComputationTokenV1,
    pub implementation: ArtifactId,
}

impl TransformIdentityV1 {
    pub fn new(name: ComputationTokenV1, implementation: ArtifactId) -> Self {
        Self {
            name,
            implementation,
        }
    }

    /// Name and hash a frozen transformer descriptor. This identifies the
    /// descriptor bytes; it does not claim that a live executable was probed.
    pub fn from_descriptor(
        name: impl Into<String>,
        descriptor: &[u8],
    ) -> Result<Self, WorldIdentityError> {
        Ok(Self::new(
            ComputationTokenV1::new(name)?,
            artifact_id_for_bytes(descriptor),
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivationInputV1 {
    pub role: ComputationTokenV1,
    pub facet: FacetIdV1,
}

impl DerivationInputV1 {
    pub fn new(role: ComputationTokenV1, facet: FacetIdV1) -> Self {
        Self { role, facet }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivationRefV1 {
    pub relation: DerivationRelationV1,
    pub inputs: Vec<DerivationInputV1>,
    pub output: FacetIdV1,
    pub transform: TransformIdentityV1,
    /// Optional reference to a `transfer_vocabulary` facet. The referenced
    /// record is descriptive only and cannot contain live authority here.
    pub transfer_contract: Option<FacetIdV1>,
}

impl DerivationRefV1 {
    pub fn new(
        relation: DerivationRelationV1,
        inputs: Vec<DerivationInputV1>,
        output: FacetIdV1,
        transform: TransformIdentityV1,
    ) -> Self {
        Self {
            relation,
            inputs,
            output,
            transform,
            transfer_contract: None,
        }
    }

    fn canonicalize(&mut self) {
        self.inputs.sort_by(|left, right| {
            left.role
                .cmp(&right.role)
                .then_with(|| left.facet.cmp(&right.facet))
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OComputationManifestV1 {
    pub schema: String,
    pub lineage: ComputationLineageId,
    pub parents: Vec<ComputationRevisionId>,
    pub roots: Vec<FacetIdV1>,
    pub facets: Vec<FacetRefV1>,
    pub derivations: Vec<DerivationRefV1>,
}

impl OComputationManifestV1 {
    pub fn new(lineage: ComputationLineageId) -> Self {
        Self {
            schema: OCOMPUTATION_MANIFEST_SCHEMA_V1.to_string(),
            lineage,
            parents: Vec::new(),
            roots: Vec::new(),
            facets: Vec::new(),
            derivations: Vec::new(),
        }
    }

    pub fn verify(self) -> Result<VerifiedOComputationV1, OComputationErrorV1> {
        VerifiedOComputationV1::verify(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<VerifiedOComputationV1, OComputationErrorV1> {
        if bytes.len() > MAX_OCOMPUTATION_MANIFEST_BYTES_V1 {
            return Err(OComputationErrorV1::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_OCOMPUTATION_MANIFEST_BYTES_V1,
            });
        }
        let manifest = decode_bounded(
            bytes,
            DecodeLimits {
                max_bytes: MAX_OCOMPUTATION_MANIFEST_BYTES_V1,
                max_items: MAX_OCOMPUTATION_DECODE_ITEMS_V1,
                max_depth: MAX_OCOMPUTATION_DECODE_DEPTH_V1,
            },
        )
        .map_err(|error| OComputationErrorV1::Canonical(error.to_string()))?;
        let verified = Self::verify(manifest)?;
        if verified.canonical_bytes()? != bytes {
            return Err(OComputationErrorV1::NonCanonicalEncoding);
        }
        Ok(verified)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<VerifiedOComputationV1, OComputationErrorV1> {
        Self::decode(bytes)
    }

    pub fn decode_json(bytes: &[u8]) -> Result<VerifiedOComputationV1, OComputationErrorV1> {
        if bytes.len() > MAX_OCOMPUTATION_MANIFEST_BYTES_V1 {
            return Err(OComputationErrorV1::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_OCOMPUTATION_MANIFEST_BYTES_V1,
            });
        }
        let manifest = serde_json::from_slice(bytes)?;
        Self::verify(manifest)
    }

    fn canonicalized(mut self) -> Self {
        self.parents.sort();
        self.roots.sort();
        self.facets.sort();
        for derivation in &mut self.derivations {
            derivation.canonicalize();
        }
        self.derivations.sort_by(|left, right| {
            left.relation
                .canonical_name()
                .cmp(right.relation.canonical_name())
                .then_with(|| left.output.cmp(&right.output))
                .then_with(|| left.inputs.cmp(&right.inputs))
                .then_with(|| left.transform.cmp(&right.transform))
                .then_with(|| left.transfer_contract.cmp(&right.transfer_contract))
        });
        self
    }

    fn validate(&self) -> Result<(), OComputationErrorV1> {
        if self.schema != OCOMPUTATION_MANIFEST_SCHEMA_V1 {
            return Err(invalid(format!(
                "unsupported schema `{}`; expected `{OCOMPUTATION_MANIFEST_SCHEMA_V1}`",
                self.schema
            )));
        }
        if self.parents.len() > MAX_OCOMPUTATION_PARENTS_V1 {
            return Err(invalid(format!(
                "parent count {} exceeds {MAX_OCOMPUTATION_PARENTS_V1}",
                self.parents.len()
            )));
        }
        if self.facets.is_empty() {
            return Err(invalid("manifest contains no facets"));
        }
        if self.facets.len() > MAX_OCOMPUTATION_FACETS_V1 {
            return Err(invalid(format!(
                "facet count {} exceeds {MAX_OCOMPUTATION_FACETS_V1}",
                self.facets.len()
            )));
        }
        if self.derivations.len() > MAX_OCOMPUTATION_DERIVATIONS_V1 {
            return Err(invalid(format!(
                "derivation count {} exceeds {MAX_OCOMPUTATION_DERIVATIONS_V1}",
                self.derivations.len()
            )));
        }

        let mut parents = BTreeSet::new();
        for parent in &self.parents {
            if is_reserved_digest(parent.artifact()) {
                return Err(invalid("parent revision uses the reserved all-zero digest"));
            }
            if !parents.insert(parent) {
                return Err(invalid(format!("duplicate parent revision `{parent}`")));
            }
        }

        let mut facets = BTreeMap::<&FacetIdV1, &FacetRefV1>::new();
        for facet in &self.facets {
            if is_reserved_digest(&facet.content) {
                return Err(invalid(format!(
                    "facet `{}` uses the reserved all-zero content digest",
                    facet.id
                )));
            }
            if let Some(previous) = facets.insert(&facet.id, facet) {
                if previous == facet {
                    return Err(OComputationErrorV1::DuplicateFacet {
                        facet: facet.id.clone(),
                    });
                }
                return Err(OComputationErrorV1::ConflictingFacet {
                    facet: facet.id.clone(),
                });
            }
        }
        if self.roots.is_empty() {
            return Err(invalid("manifest declares no root facets"));
        }
        if self.roots.len() > MAX_OCOMPUTATION_FACETS_V1 {
            return Err(invalid(format!(
                "root count {} exceeds {MAX_OCOMPUTATION_FACETS_V1}",
                self.roots.len()
            )));
        }
        let mut roots = BTreeSet::new();
        for root in &self.roots {
            if !facets.contains_key(root) {
                return Err(invalid(format!(
                    "manifest names missing root facet `{root}`"
                )));
            }
            if !roots.insert(root) {
                return Err(invalid(format!("duplicate root facet `{root}`")));
            }
        }

        let mut producer = BTreeMap::<&FacetIdV1, usize>::new();
        let mut adjacency = BTreeMap::<&FacetIdV1, Vec<&FacetIdV1>>::new();
        let mut indegree = facets
            .keys()
            .copied()
            .map(|id| (id, 0usize))
            .collect::<BTreeMap<_, _>>();

        for (index, derivation) in self.derivations.iter().enumerate() {
            if derivation.inputs.is_empty() {
                return Err(invalid(format!(
                    "derivation {index} for `{}` has no inputs",
                    derivation.output
                )));
            }
            if derivation.inputs.len() > MAX_OCOMPUTATION_DERIVATION_INPUTS_V1 {
                return Err(invalid(format!(
                    "derivation {index} input count {} exceeds {MAX_OCOMPUTATION_DERIVATION_INPUTS_V1}",
                    derivation.inputs.len()
                )));
            }
            if is_reserved_digest(&derivation.transform.implementation) {
                return Err(invalid(format!(
                    "derivation {index} transformer uses the reserved all-zero digest"
                )));
            }
            let Some(_output) = facets.get(&derivation.output) else {
                return Err(invalid(format!(
                    "derivation {index} names missing output facet `{}`",
                    derivation.output
                )));
            };
            if roots.contains(&derivation.output) {
                return Err(invalid(format!(
                    "root facet `{}` must not be produced by a derivation",
                    derivation.output
                )));
            }
            if let Some(previous) = producer.insert(&derivation.output, index) {
                let _ = previous;
                return Err(OComputationErrorV1::MultipleProducers {
                    facet: derivation.output.clone(),
                });
            }

            let mut unique_roles = BTreeSet::new();
            let mut dependency_facets = BTreeSet::new();
            for input in &derivation.inputs {
                if !facets.contains_key(&input.facet) {
                    return Err(invalid(format!(
                        "derivation {index} names missing input facet `{}`",
                        input.facet
                    )));
                }
                if input.facet == derivation.output {
                    return Err(invalid(format!(
                        "derivation {index} directly derives facet `{}` from itself",
                        input.facet
                    )));
                }
                if !unique_roles.insert(&input.role) {
                    return Err(invalid(format!(
                        "derivation {index} repeats input role `{}`",
                        input.role
                    )));
                }
                dependency_facets.insert(&input.facet);
            }
            if let Some(contract) = &derivation.transfer_contract {
                let Some(contract_facet) = facets.get(contract) else {
                    return Err(invalid(format!(
                        "derivation {index} names missing transfer contract facet `{contract}`"
                    )));
                };
                if contract_facet.kind != FacetKindV1::TransferVocabulary {
                    return Err(invalid(format!(
                        "derivation {index} transfer contract `{contract}` is not a transfer_vocabulary facet"
                    )));
                }
                dependency_facets.insert(contract);
            }

            for input in dependency_facets {
                adjacency.entry(input).or_default().push(&derivation.output);
                let degree = indegree
                    .get_mut(&derivation.output)
                    .expect("validated output facet has an indegree slot");
                *degree = degree
                    .checked_add(1)
                    .ok_or_else(|| invalid("derivation indegree overflow"))?;
            }
        }

        for facet in &self.facets {
            let produced = producer.contains_key(&facet.id);
            if roots.contains(&facet.id) && produced {
                return Err(invalid(format!(
                    "root facet `{}` unexpectedly has a producer",
                    facet.id
                )));
            }
            if !roots.contains(&facet.id) && !produced {
                return Err(OComputationErrorV1::OrphanFacet {
                    facet: facet.id.clone(),
                });
            }
        }

        let mut ready = indegree
            .iter()
            .filter_map(|(facet, degree)| (*degree == 0).then_some(*facet))
            .collect::<VecDeque<_>>();
        let mut visited = 0usize;
        while let Some(facet) = ready.pop_front() {
            visited += 1;
            if let Some(outputs) = adjacency.get(facet) {
                for output in outputs {
                    let degree = indegree
                        .get_mut(output)
                        .expect("validated output facet has an indegree slot");
                    *degree -= 1;
                    if *degree == 0 {
                        ready.push_back(output);
                    }
                }
            }
        }
        if visited != facets.len() {
            return Err(OComputationErrorV1::DerivationCycle);
        }

        Ok(())
    }
}

/// Verified, canonical, authority-free computation description.
///
/// Private fields prevent callers from pairing an unchecked manifest with a
/// counterfeit revision. Decoding this type never reconstructs a capability,
/// lease, signer, process handle, or `AdmittedExecution`.
///
/// ```compile_fail
/// use ostadix_api::computation_core::VerifiedOComputationV1;
/// use ostadix_api::evidence::AdmittedExecution;
///
/// fn forbidden_relabel<'a>(
///     decoded: VerifiedOComputationV1,
/// ) -> AdmittedExecution<'a> {
///     decoded.into()
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedOComputationV1 {
    manifest: OComputationManifestV1,
    revision: ComputationRevisionId,
}

impl VerifiedOComputationV1 {
    pub fn verify(manifest: OComputationManifestV1) -> Result<Self, OComputationErrorV1> {
        manifest.validate()?;
        let manifest = manifest.canonicalized();
        manifest.validate()?;
        let bytes =
            encode(&manifest).map_err(|error| OComputationErrorV1::Canonical(error.to_string()))?;
        if bytes.len() > MAX_OCOMPUTATION_MANIFEST_BYTES_V1 {
            return Err(OComputationErrorV1::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_OCOMPUTATION_MANIFEST_BYTES_V1,
            });
        }
        let revision = revision_for_canonical_bytes(&bytes)?;
        Ok(Self { manifest, revision })
    }

    pub fn manifest(&self) -> &OComputationManifestV1 {
        &self.manifest
    }

    pub fn revision(&self) -> &ComputationRevisionId {
        &self.revision
    }

    pub fn facet(&self, id: &FacetIdV1) -> Option<&FacetRefV1> {
        self.manifest.facets.iter().find(|facet| &facet.id == id)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        let bytes = encode(&self.manifest)
            .map_err(|error| OComputationErrorV1::Canonical(error.to_string()))?;
        if bytes.len() > MAX_OCOMPUTATION_MANIFEST_BYTES_V1 {
            return Err(OComputationErrorV1::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_OCOMPUTATION_MANIFEST_BYTES_V1,
            });
        }
        Ok(bytes)
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        Ok(serde_json::to_vec(&self.manifest)?)
    }

    pub fn canonical_json_pretty(&self) -> Result<Vec<u8>, OComputationErrorV1> {
        let mut bytes = serde_json::to_vec_pretty(&self.manifest)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn require_facet_bytes(
        &self,
        id: &FacetIdV1,
        bytes: &[u8],
    ) -> Result<(), OComputationErrorV1> {
        let facet = self
            .facet(id)
            .ok_or_else(|| invalid(format!("manifest has no facet `{id}`")))?;
        let actual = artifact_id_for_bytes(bytes);
        if actual != facet.content {
            return Err(OComputationErrorV1::FacetContentMismatch {
                facet: id.clone(),
                expected: facet.content.clone(),
                actual,
            });
        }
        Ok(())
    }
}

pub fn artifact_id_for_bytes(bytes: &[u8]) -> ArtifactId {
    ArtifactId::from_sha256(hex::encode(Sha256::digest(bytes)))
        .expect("SHA-256 hex output is always a valid ArtifactId")
}

fn revision_for_canonical_bytes(
    canonical: &[u8],
) -> Result<ComputationRevisionId, OComputationErrorV1> {
    let length = u64::try_from(canonical.len())
        .map_err(|_| invalid("canonical computation length does not fit u64"))?;
    let mut hasher = Sha256::new();
    hasher.update(OCOMPUTATION_REVISION_DIGEST_DOMAIN_V1);
    hasher.update(length.to_be_bytes());
    hasher.update(canonical);
    ComputationRevisionId::from_artifact(ArtifactId::from_sha256(hex::encode(hasher.finalize()))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(value: &str) -> ComputationTokenV1 {
        ComputationTokenV1::new(value).unwrap()
    }

    fn facet_id(value: &str) -> FacetIdV1 {
        FacetIdV1::new(value).unwrap()
    }

    fn facet(id: &str, kind: FacetKindV1, bytes: &[u8]) -> FacetRefV1 {
        FacetRefV1::new(
            facet_id(id),
            kind,
            token("test.facet/v1"),
            artifact_id_for_bytes(bytes),
        )
    }

    fn transform(bytes: &[u8]) -> TransformIdentityV1 {
        TransformIdentityV1::new(token("test/transform/v1"), artifact_id_for_bytes(bytes))
    }

    fn input(role: &str, facet: &str) -> DerivationInputV1 {
        DerivationInputV1::new(token(role), facet_id(facet))
    }

    fn manifest(source: &[u8], graph: &[u8], transformer: &[u8]) -> OComputationManifestV1 {
        let mut manifest =
            OComputationManifestV1::new(ComputationLineageId::new("tests/computation").unwrap());
        manifest.facets = vec![
            facet("source", FacetKindV1::Source, source),
            facet("plan", FacetKindV1::ExecutionPlan, b"plan"),
            facet("graph", FacetKindV1::SolvedHgraph, graph),
        ];
        manifest.roots = vec![facet_id("source")];
        manifest.derivations = vec![
            DerivationRefV1::new(
                DerivationRelationV1::PlannedFrom,
                vec![input("source", "source")],
                facet_id("plan"),
                transform(b"planner"),
            ),
            DerivationRefV1::new(
                DerivationRelationV1::SolvedFrom,
                vec![input("source", "source"), input("plan", "plan")],
                facet_id("graph"),
                transform(transformer),
            ),
        ];
        manifest
    }

    fn semantic_ref(name: &str) -> SemanticArtifactRefV1 {
        SemanticArtifactRefV1::new(
            token(&format!("test/{name}/v1")),
            artifact_id_for_bytes(name.as_bytes()),
        )
        .unwrap()
    }

    fn operation_port(name: &str, value_type: &str) -> OperationPortV1 {
        OperationPortV1::new(token(name), semantic_ref(value_type)).unwrap()
    }

    fn operation_contract() -> OperationContractV1 {
        OperationContractV1::new(
            OperationIdV1::new("image/resize").unwrap(),
            1,
            semantic_ref("preconditions"),
            semantic_ref("postconditions"),
            semantic_ref("state-model"),
            semantic_ref("effect-model"),
            semantic_ref("ordering"),
            semantic_ref("determinism"),
            semantic_ref("required-fidelity"),
        )
        .unwrap()
    }

    fn operation_interface(contract: &OperationContractV1) -> OperationInterfaceV1 {
        OperationInterfaceV1::new(
            contract.operation.clone(),
            contract.semantic_version,
            contract.id().unwrap(),
            vec![OperationShapeParameterV1::new(
                token("channels"),
                semantic_ref("positive-dimension"),
            )
            .unwrap()],
            vec![
                operation_port("width", "unsigned-width"),
                operation_port("image", "image-tensor"),
            ],
            vec![operation_port("result", "image-tensor")],
        )
        .unwrap()
    }

    fn realization_descriptor(
        contract: &OperationContractV1,
        interface: &OperationInterfaceV1,
        stable_name: &str,
        implementation: &str,
    ) -> RealizationDescriptorV1 {
        RealizationDescriptorV1::new(
            RealizationIdV1::new(stable_name).unwrap(),
            interface.id().unwrap(),
            contract.id().unwrap(),
            artifact_id_for_bytes(implementation.as_bytes()),
            semantic_ref("execution-pipeline"),
            vec![
                RealizationPortRepresentationsV1::new(token("width"), vec![semantic_ref("u64-le")])
                    .unwrap(),
                RealizationPortRepresentationsV1::new(
                    token("image"),
                    vec![semantic_ref("rgb-planar"), semantic_ref("rgb-packed")],
                )
                .unwrap(),
            ],
            vec![RealizationPortRepresentationsV1::new(
                token("result"),
                vec![semantic_ref("rgb-packed")],
            )
            .unwrap()],
            semantic_ref("target-requirements"),
            semantic_ref("state-requirements"),
            semantic_ref("actor-requirements"),
            semantic_ref("supplied-fidelity"),
            Some(semantic_ref("cost-model")),
            vec![semantic_ref("evidence-z"), semantic_ref("evidence-a")],
        )
        .unwrap()
    }

    fn realization_fixture() -> (
        OperationContractV1,
        OperationInterfaceV1,
        Vec<RealizationDescriptorV1>,
        RealizationSetV1,
    ) {
        let contract = operation_contract();
        let interface = operation_interface(&contract);
        let descriptors = vec![
            realization_descriptor(&contract, &interface, "native/simd", "simd-v1"),
            realization_descriptor(&contract, &interface, "portable/scalar", "scalar-v1"),
        ];
        let realization_set = RealizationSetV1::new(
            interface.id().unwrap(),
            contract.id().unwrap(),
            descriptors
                .iter()
                .rev()
                .map(|descriptor| descriptor.id().unwrap())
                .collect(),
        )
        .unwrap();
        (contract, interface, descriptors, realization_set)
    }

    #[test]
    fn exact_inputs_produce_one_stable_revision() {
        let first = manifest(b"source", b"graph", b"solver").verify().unwrap();
        let second = manifest(b"source", b"graph", b"solver").verify().unwrap();
        assert_eq!(first.revision(), second.revision());
        assert_eq!(
            first.canonical_bytes().unwrap(),
            second.canonical_bytes().unwrap()
        );
    }

    #[test]
    fn source_graph_and_transformer_are_revision_bound() {
        let base = manifest(b"source", b"graph", b"solver").verify().unwrap();
        let source = manifest(b"source-2", b"graph", b"solver").verify().unwrap();
        let graph = manifest(b"source", b"graph-2", b"solver").verify().unwrap();
        let transformer = manifest(b"source", b"graph", b"solver-2").verify().unwrap();
        assert_ne!(base.revision(), source.revision());
        assert_ne!(base.revision(), graph.revision());
        assert_ne!(base.revision(), transformer.revision());
    }

    #[test]
    fn facet_and_derivation_order_are_canonicalized() {
        let left = manifest(b"source", b"graph", b"solver").verify().unwrap();
        let mut reordered = manifest(b"source", b"graph", b"solver");
        reordered.facets.reverse();
        reordered.derivations.reverse();
        reordered.derivations[0].inputs.reverse();
        let right = reordered.verify().unwrap();
        assert_eq!(left, right);
    }

    #[test]
    fn orphan_derived_facet_is_rejected() {
        let mut orphan = manifest(b"source", b"graph", b"solver");
        orphan.derivations.clear();
        let error = orphan.verify().unwrap_err();
        assert!(matches!(error, OComputationErrorV1::OrphanFacet { .. }));
    }

    #[test]
    fn derivation_cycle_is_rejected() {
        let mut cycle =
            OComputationManifestV1::new(ComputationLineageId::new("tests/cycle").unwrap());
        cycle.facets = vec![
            facet("source", FacetKindV1::Source, b"source"),
            facet("left", FacetKindV1::ExecutionPlan, b"left"),
            facet("right", FacetKindV1::SolvedHgraph, b"right"),
        ];
        cycle.roots = vec![facet_id("source")];
        cycle.derivations = vec![
            DerivationRefV1::new(
                DerivationRelationV1::PlannedFrom,
                vec![input("right", "right")],
                facet_id("left"),
                transform(b"left-transform"),
            ),
            DerivationRefV1::new(
                DerivationRelationV1::SolvedFrom,
                vec![input("left", "left")],
                facet_id("right"),
                transform(b"right-transform"),
            ),
        ];
        let error = cycle.verify().unwrap_err();
        assert!(matches!(error, OComputationErrorV1::DerivationCycle));
    }

    #[test]
    fn transfer_contract_dependency_cycle_is_rejected() {
        let mut cycle = OComputationManifestV1::new(
            ComputationLineageId::new("tests/transfer-contract-cycle").unwrap(),
        );
        cycle.facets = vec![
            facet("source", FacetKindV1::Source, b"source"),
            facet("result", FacetKindV1::ExecutionPlan, b"result"),
            facet("contract", FacetKindV1::TransferVocabulary, b"contract"),
        ];
        cycle.roots = vec![facet_id("source")];
        let mut result = DerivationRefV1::new(
            DerivationRelationV1::PlannedFrom,
            vec![input("source", "source")],
            facet_id("result"),
            transform(b"result-transform"),
        );
        result.transfer_contract = Some(facet_id("contract"));
        cycle.derivations = vec![
            result,
            DerivationRefV1::new(
                DerivationRelationV1::ProjectedFrom,
                vec![input("result", "result")],
                facet_id("contract"),
                transform(b"contract-transform"),
            ),
        ];

        let error = cycle.verify().unwrap_err();
        assert!(matches!(error, OComputationErrorV1::DerivationCycle));
    }

    #[test]
    fn conflicting_duplicate_facet_identity_is_rejected() {
        let mut duplicate = manifest(b"source", b"graph", b"solver");
        duplicate
            .facets
            .push(facet("graph", FacetKindV1::SolvedHgraph, b"other"));
        let error = duplicate.verify().unwrap_err();
        assert!(matches!(
            error,
            OComputationErrorV1::ConflictingFacet { .. }
        ));
    }

    #[test]
    fn exact_duplicate_facet_is_rejected_without_deduplication() {
        let mut duplicate = manifest(b"source", b"graph", b"solver");
        duplicate.facets.push(duplicate.facets[0].clone());
        let error = duplicate.verify().unwrap_err();
        assert!(matches!(error, OComputationErrorV1::DuplicateFacet { .. }));
    }

    #[test]
    fn duplicate_derivation_producer_is_rejected() {
        let mut duplicate = manifest(b"source", b"graph", b"solver");
        duplicate.derivations.push(duplicate.derivations[1].clone());
        let error = duplicate.verify().unwrap_err();
        assert!(matches!(
            error,
            OComputationErrorV1::MultipleProducers { .. }
        ));
    }

    #[test]
    fn canonical_cbor_round_trip_preserves_revision() {
        let verified = manifest(b"source", b"graph", b"solver").verify().unwrap();
        let bytes = verified.canonical_bytes().unwrap();
        let decoded = OComputationManifestV1::decode_canonical(&bytes).unwrap();
        assert_eq!(decoded, verified);
    }

    #[test]
    fn canonical_cbor_and_revision_vector_are_pinned() {
        let verified = manifest(b"source", b"graph", b"solver").verify().unwrap();
        let bytes = verified.canonical_bytes().unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            "4c87cc59c5495b6f6752f80da2185cdaf6a29cfd7312c836bafb4a6bb00ea74d"
        );
        assert_eq!(
            verified.revision().as_sha256(),
            "510685b6126fee749934f83fd8b73e5850039b03be64f9a790b30f3eef7741ac"
        );
    }

    #[test]
    fn every_cbor_decode_rejects_equivalent_unsorted_vectors() {
        let mut unsorted = manifest(b"source", b"graph", b"solver");
        unsorted.facets.reverse();
        unsorted.derivations.reverse();
        unsorted.derivations[0].inputs.reverse();
        unsorted.validate().unwrap();
        let noncanonical = encode(&unsorted).unwrap();
        for error in [
            OComputationManifestV1::decode(&noncanonical).unwrap_err(),
            OComputationManifestV1::decode_canonical(&noncanonical).unwrap_err(),
        ] {
            assert!(matches!(error, OComputationErrorV1::NonCanonicalEncoding));
        }
    }

    #[test]
    fn revision_nominal_type_rejects_reserved_digest_during_decode() {
        let reserved = format!("\"{}\"", "0".repeat(64));
        let error = serde_json::from_str::<ComputationRevisionId>(&reserved).unwrap_err();
        assert!(error.to_string().contains("reserved all-zero digest"));
    }

    #[test]
    fn unknown_json_field_is_rejected() {
        let verified = manifest(b"source", b"graph", b"solver").verify().unwrap();
        let mut value = serde_json::to_value(verified.manifest()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("authority".to_string(), serde_json::Value::Bool(true));
        let error =
            OComputationManifestV1::decode_json(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(matches!(error, OComputationErrorV1::Json(_)));
    }

    #[test]
    fn decoded_admission_reference_is_descriptive_not_authority() {
        let mut manifest = manifest(b"source", b"graph", b"solver");
        manifest.facets.push(facet(
            "admission",
            FacetKindV1::AdmissionRecord,
            b"descriptive-admission-record",
        ));
        manifest.derivations.push(DerivationRefV1::new(
            DerivationRelationV1::AdmittedFrom,
            vec![input("graph", "graph")],
            facet_id("admission"),
            transform(b"admission-analyzer"),
        ));
        let verified = manifest.verify().unwrap();
        let decoded =
            OComputationManifestV1::decode_canonical(&verified.canonical_bytes().unwrap()).unwrap();
        assert_eq!(decoded.revision(), verified.revision());
        assert_eq!(
            decoded.facet(&facet_id("admission")).unwrap().kind,
            FacetKindV1::AdmissionRecord
        );
        // The decoded type exposes only identity, bytes, and facet inspection;
        // no constructor in this module can mint a capability or admission.
    }

    #[test]
    fn operation_realization_facet_kind_wire_spellings_are_frozen() {
        for (kind, spelling) in [
            (FacetKindV1::OperationContract, "operation_contract"),
            (FacetKindV1::OperationInterface, "operation_interface"),
            (FacetKindV1::RealizationDescriptor, "realization_descriptor"),
            (FacetKindV1::RealizationSet, "realization_set"),
            (
                FacetKindV1::PhysicalRepresentation,
                "physical_representation",
            ),
            (FacetKindV1::TransferPlan, "transfer_plan"),
            (FacetKindV1::CostProfile, "cost_profile"),
            (FacetKindV1::Objective, "objective"),
            (FacetKindV1::RecoveryPlan, "recovery_plan"),
        ] {
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{spelling}\"")
            );
            assert_eq!(encode(&kind).unwrap(), encode(&spelling).unwrap());
        }
    }

    #[test]
    fn exact_facet_bytes_are_rechecked() {
        let verified = manifest(b"source", b"graph", b"solver").verify().unwrap();
        verified
            .require_facet_bytes(&facet_id("source"), b"source")
            .unwrap();
        let error = verified
            .require_facet_bytes(&facet_id("source"), b"changed")
            .unwrap_err()
            .to_string();
        assert!(error.contains("content mismatch"), "{error}");
    }

    #[test]
    fn operation_realization_records_round_trip_through_canonical_cbor_and_json() {
        let (contract, interface, descriptors, realization_set) = realization_fixture();

        let contract_bytes = contract.canonical_bytes().unwrap();
        assert_eq!(
            OperationContractV1::decode_canonical(&contract_bytes).unwrap(),
            contract
        );
        assert_eq!(
            OperationContractV1::decode_json(&contract.canonical_json().unwrap()).unwrap(),
            contract
        );

        let interface_bytes = interface.canonical_bytes().unwrap();
        assert_eq!(
            OperationInterfaceV1::decode_canonical(&interface_bytes).unwrap(),
            interface
        );
        assert_eq!(
            OperationInterfaceV1::decode_json(&interface.canonical_json().unwrap()).unwrap(),
            interface
        );

        for descriptor in &descriptors {
            assert_eq!(
                RealizationDescriptorV1::decode_canonical(&descriptor.canonical_bytes().unwrap())
                    .unwrap(),
                *descriptor
            );
            assert_eq!(
                RealizationDescriptorV1::decode_json(&descriptor.canonical_json().unwrap())
                    .unwrap(),
                *descriptor
            );
        }

        assert_eq!(
            RealizationSetV1::decode_canonical(&realization_set.canonical_bytes().unwrap())
                .unwrap(),
            realization_set
        );
        assert_eq!(
            RealizationSetV1::decode_json(&realization_set.canonical_json().unwrap()).unwrap(),
            realization_set
        );
        assert!(realization_set
            .canonical_json_pretty()
            .unwrap()
            .ends_with(b"\n"));
    }

    #[test]
    fn operation_realization_vector_order_is_canonicalized_without_deduplication() {
        let (_contract, interface, descriptors, realization_set) = realization_fixture();

        let mut reordered_interface = interface.clone();
        reordered_interface.inputs.reverse();
        assert_eq!(reordered_interface.clone().verify().unwrap(), interface);
        let noncanonical = encode(&reordered_interface).unwrap();
        assert!(matches!(
            OperationInterfaceV1::decode_canonical(&noncanonical).unwrap_err(),
            OComputationErrorV1::NonCanonicalSemanticEncoding {
                record: "OperationInterfaceV1"
            }
        ));
        assert_eq!(
            OperationInterfaceV1::decode_json(&serde_json::to_vec(&reordered_interface).unwrap())
                .unwrap(),
            interface
        );

        let mut reordered_descriptor = descriptors[0].clone();
        reordered_descriptor.input_representations.reverse();
        reordered_descriptor.input_representations[0]
            .representations
            .reverse();
        reordered_descriptor.validation_evidence.reverse();
        let noncanonical = encode(&reordered_descriptor).unwrap();
        assert!(matches!(
            RealizationDescriptorV1::decode_canonical(&noncanonical).unwrap_err(),
            OComputationErrorV1::NonCanonicalSemanticEncoding {
                record: "RealizationDescriptorV1"
            }
        ));
        assert_eq!(reordered_descriptor.verify().unwrap(), descriptors[0]);

        let mut reordered_set = realization_set.clone();
        reordered_set.realizations.reverse();
        let noncanonical = encode(&reordered_set).unwrap();
        assert!(matches!(
            RealizationSetV1::decode_canonical(&noncanonical).unwrap_err(),
            OComputationErrorV1::NonCanonicalSemanticEncoding {
                record: "RealizationSetV1"
            }
        ));
        assert_eq!(reordered_set.verify().unwrap(), realization_set);

        let mut duplicate_interface = interface;
        duplicate_interface
            .inputs
            .push(duplicate_interface.inputs[0].clone());
        assert!(duplicate_interface.verify().is_err());
    }

    #[test]
    fn operation_realization_ids_are_domain_separated() {
        let (contract, interface, descriptors, realization_set) = realization_fixture();
        let contract_id = contract.id().unwrap();
        let interface_id = interface.id().unwrap();
        let descriptor_id = descriptors[0].id().unwrap();
        let realization_set_id = realization_set.id().unwrap();

        let same_contract_other_domain =
            semantic_record_artifact_id_v1(&contract, OPERATION_INTERFACE_DIGEST_DOMAIN_V1)
                .unwrap();
        assert_ne!(
            contract_id.as_sha256(),
            same_contract_other_domain.as_sha256()
        );
        assert_ne!(contract_id.as_sha256(), interface_id.as_sha256());
        assert_ne!(interface_id.as_sha256(), descriptor_id.as_sha256());
        assert_ne!(descriptor_id.as_sha256(), realization_set_id.as_sha256());
    }

    #[test]
    fn operation_realization_id_vectors_are_pinned() {
        let (contract, interface, descriptors, realization_set) = realization_fixture();
        assert_eq!(
            contract.id().unwrap().as_sha256(),
            "fc3609ca05559611f009cd8da27a194112aadfd8826670b0528630c6b043a614"
        );
        assert_eq!(
            interface.id().unwrap().as_sha256(),
            "0af99c268becec72bff1c844bd98ca1caffc7d17aff77c806ef85752e367e887"
        );
        assert_eq!(
            descriptors[0].id().unwrap().as_sha256(),
            "fe52ed7724f7d2509adcf6311cb70078467e74cf260e8147c0aaf9381f27246a"
        );
        assert_eq!(
            realization_set.id().unwrap().as_sha256(),
            "9f2225704ee66dd352e52749d6f094961d2ce4377f6699251b63086656c1eb36"
        );
    }

    #[test]
    fn operation_realization_facet_refs_use_raw_content_hashes() {
        let (contract, interface, descriptors, realization_set) = realization_fixture();
        let fixtures = [
            (
                contract.facet_ref(facet_id("contract")).unwrap(),
                FacetKindV1::OperationContract,
                OPERATION_CONTRACT_SCHEMA_V1,
                contract.canonical_bytes().unwrap(),
                contract.id().unwrap().as_sha256().to_string(),
            ),
            (
                interface.facet_ref(facet_id("interface")).unwrap(),
                FacetKindV1::OperationInterface,
                OPERATION_INTERFACE_SCHEMA_V1,
                interface.canonical_bytes().unwrap(),
                interface.id().unwrap().as_sha256().to_string(),
            ),
            (
                descriptors[0].facet_ref(facet_id("descriptor")).unwrap(),
                FacetKindV1::RealizationDescriptor,
                REALIZATION_DESCRIPTOR_SCHEMA_V1,
                descriptors[0].canonical_bytes().unwrap(),
                descriptors[0].id().unwrap().as_sha256().to_string(),
            ),
            (
                realization_set.facet_ref(facet_id("set")).unwrap(),
                FacetKindV1::RealizationSet,
                REALIZATION_SET_SCHEMA_V1,
                realization_set.canonical_bytes().unwrap(),
                realization_set.id().unwrap().as_sha256().to_string(),
            ),
        ];

        for (facet, kind, schema, bytes, semantic_id) in fixtures {
            assert_eq!(facet.kind, kind);
            assert_eq!(facet.schema.as_str(), schema);
            assert_eq!(facet.content, artifact_id_for_bytes(&bytes));
            assert_ne!(facet.content.as_sha256(), semantic_id.as_str());
        }
    }

    #[test]
    fn operation_realization_records_reject_invalid_and_oversized_inputs() {
        let zero = ArtifactId::from_sha256("0".repeat(64)).unwrap();
        assert!(SemanticArtifactRefV1::new(token("test/zero/v1"), zero).is_err());
        let zero_json = format!("\"{}\"", "0".repeat(64));
        assert!(serde_json::from_str::<OperationContractIdV1>(&zero_json).is_err());
        assert!(serde_json::from_str::<OperationInterfaceIdV1>(&zero_json).is_err());
        assert!(serde_json::from_str::<RealizationDescriptorIdV1>(&zero_json).is_err());
        assert!(serde_json::from_str::<RealizationSetIdV1>(&zero_json).is_err());

        let (contract, mut interface, mut descriptors, mut realization_set) = realization_fixture();
        let mut bad_schema = contract;
        bad_schema.schema = "ostadix.operation-contract/v2".to_string();
        assert!(bad_schema.verify().is_err());

        interface.semantic_version = 0;
        assert!(interface.verify().is_err());

        descriptors[0].input_representations[0]
            .representations
            .clear();
        assert!(descriptors.remove(0).verify().is_err());

        realization_set.realizations.clear();
        assert!(realization_set.verify().is_err());

        let oversized = vec![b' '; MAX_OPERATION_SEMANTIC_RECORD_BYTES_V1 + 1];
        assert!(matches!(
            OperationContractV1::decode_json(&oversized).unwrap_err(),
            OComputationErrorV1::SemanticRecordTooLarge {
                record: "OperationContractV1",
                ..
            }
        ));
        assert!(matches!(
            OperationContractV1::decode_canonical(&oversized).unwrap_err(),
            OComputationErrorV1::SemanticRecordTooLarge {
                record: "OperationContractV1",
                ..
            }
        ));
    }

    #[test]
    fn operation_errors_name_record_families_without_changing_manifest_errors() {
        let mut contract = operation_contract();
        contract.semantic_version = 0;
        let error = contract.verify().unwrap_err();
        let rendered = error.to_string();
        assert!(matches!(
            error,
            OComputationErrorV1::InvalidSemanticRecord {
                record: "OperationContractV1",
                ..
            }
        ));
        assert!(rendered.starts_with("invalid OperationContractV1:"));

        let error = OperationContractV1::decode_canonical(b"not canonical CBOR").unwrap_err();
        assert!(matches!(
            error,
            OComputationErrorV1::SemanticCanonical {
                record: "OperationContractV1",
                ..
            }
        ));

        let mut legacy_manifest = manifest(b"source", b"graph", b"solver");
        legacy_manifest.schema = "ostadix.ocomputation-manifest/v2".to_string();
        let error = legacy_manifest.verify().unwrap_err();
        let rendered = error.to_string();
        assert!(matches!(error, OComputationErrorV1::Invalid(_)));
        assert!(rendered.starts_with("invalid OComputationManifestV1:"));
    }

    #[test]
    fn operation_semantic_json_rejects_unknown_fields() {
        let contract = operation_contract();
        let mut value = serde_json::to_value(contract).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("authority".to_string(), serde_json::Value::Bool(true));
        let error =
            OperationContractV1::decode_json(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(matches!(
            error,
            OComputationErrorV1::SemanticJson {
                record: "OperationContractV1",
                ..
            }
        ));
    }

    #[test]
    fn realization_set_count_preflight_rejects_oversize_and_mismatch() {
        let over_limit = MAX_REALIZATION_SET_MEMBERS_V1 + 1;
        for error in [
            validate_realization_closure_counts_v1(over_limit, over_limit).unwrap_err(),
            validate_realization_closure_counts_v1(1, over_limit).unwrap_err(),
        ] {
            let rendered = error.to_string();
            assert!(matches!(
                error,
                OComputationErrorV1::InvalidSemanticRecord {
                    record: "operation realization closure V1",
                    ..
                }
            ));
            assert!(rendered.contains("exceeds"));
        }

        let error = validate_realization_closure_counts_v1(1, 2).unwrap_err();
        assert!(error.to_string().contains("membership count"));
    }

    #[test]
    fn realization_set_cross_record_verification_accepts_exact_membership() {
        let (contract, interface, descriptors, realization_set) = realization_fixture();
        verify_realization_set_v1(&contract, &interface, &descriptors, &realization_set).unwrap();
    }

    #[test]
    fn realization_set_cross_record_verification_rejects_contract_mismatches() {
        let (contract, interface, descriptors, realization_set) = realization_fixture();

        let mut wrong_operation = contract.clone();
        wrong_operation.operation = OperationIdV1::new("image/crop").unwrap();
        assert!(verify_realization_set_v1(
            &wrong_operation,
            &interface,
            &descriptors,
            &realization_set
        )
        .unwrap_err()
        .to_string()
        .contains("interface names"));

        let mut wrong_version = interface.clone();
        wrong_version.semantic_version = 2;
        assert!(verify_realization_set_v1(
            &contract,
            &wrong_version,
            &descriptors,
            &realization_set
        )
        .unwrap_err()
        .to_string()
        .contains("semantic_version"));

        let mut wrong_contract = interface;
        wrong_contract.contract =
            OperationContractIdV1::from_artifact(artifact_id_for_bytes(b"other-contract")).unwrap();
        assert!(verify_realization_set_v1(
            &contract,
            &wrong_contract,
            &descriptors,
            &realization_set
        )
        .unwrap_err()
        .to_string()
        .contains("supplied contract"));
    }

    #[test]
    fn realization_set_cross_record_verification_rejects_port_and_membership_drift() {
        let (contract, interface, descriptors, realization_set) = realization_fixture();

        let mut missing_input = descriptors[0].clone();
        missing_input.input_representations.remove(0);
        let port_drift_set = RealizationSetV1::new(
            interface.id().unwrap(),
            contract.id().unwrap(),
            vec![missing_input.id().unwrap()],
        )
        .unwrap();
        assert!(verify_realization_set_v1(
            &contract,
            &interface,
            &[missing_input],
            &port_drift_set
        )
        .unwrap_err()
        .to_string()
        .contains("exactly cover"));

        assert!(verify_realization_set_v1(
            &contract,
            &interface,
            &descriptors[..1],
            &realization_set
        )
        .unwrap_err()
        .to_string()
        .contains("membership"));

        let mut wrong_interface = descriptors[0].clone();
        wrong_interface.interface =
            OperationInterfaceIdV1::from_artifact(artifact_id_for_bytes(b"other-interface"))
                .unwrap();
        let wrong_interface_set = RealizationSetV1::new(
            interface.id().unwrap(),
            contract.id().unwrap(),
            vec![wrong_interface.id().unwrap()],
        )
        .unwrap();
        assert!(verify_realization_set_v1(
            &contract,
            &interface,
            &[wrong_interface],
            &wrong_interface_set
        )
        .unwrap_err()
        .to_string()
        .contains("supplied interface"));
    }

    #[test]
    fn realization_set_cross_record_verification_rejects_duplicate_stable_names() {
        let (contract, interface, descriptors, _) = realization_fixture();
        let first = descriptors[0].clone();
        let mut second = first.clone();
        second.implementation = artifact_id_for_bytes(b"distinct-implementation");
        let second = second.verify().unwrap();
        let realization_set = RealizationSetV1::new(
            interface.id().unwrap(),
            contract.id().unwrap(),
            vec![first.id().unwrap(), second.id().unwrap()],
        )
        .unwrap();
        assert!(verify_realization_set_v1(
            &contract,
            &interface,
            &[first, second],
            &realization_set
        )
        .unwrap_err()
        .to_string()
        .contains("duplicate stable realization name"));
    }
}
