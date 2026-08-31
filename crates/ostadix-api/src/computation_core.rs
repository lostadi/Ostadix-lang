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

const MAX_OCOMPUTATION_DECODE_ITEMS_V1: usize = 1_000_000;
const MAX_OCOMPUTATION_DECODE_DEPTH_V1: usize = 64;
const OCOMPUTATION_REVISION_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/O-COMPUTATION-REVISION/V1\0";

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OComputationErrorV1 {
    #[error("invalid OComputationManifestV1: {0}")]
    Invalid(String),
    #[error("OComputation canonical encoding failed: {0}")]
    Canonical(String),
    #[error("OComputation JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Identity(#[from] WorldIdentityError),
    #[error("OComputation record is {actual} bytes; maximum is {maximum}")]
    RecordTooLarge { actual: usize, maximum: usize },
    #[error("OComputation bytes are not the canonical encoding")]
    NonCanonicalEncoding,
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
}
