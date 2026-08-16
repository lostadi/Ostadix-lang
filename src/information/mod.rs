//! Authority-free information identity, provenance, and projection records.
//!
//! V1 is deliberately a sidecar substrate.  Membership in an information
//! snapshot does not grant execution authority and does not imply that two
//! records are causally related.  Existing evidence, admission, placement,
//! Hosted, World, and kernel authorities remain responsible for their own
//! validation and enforcement.

mod acquisition;
mod decision;
mod delta;
mod exchange;
mod id;
mod invalidation;
mod loss;
mod model;
mod projection;
mod root;
mod store;

pub use acquisition::{select_candidate_v1, AcquisitionBudgetV1, AcquisitionCandidateV1};
pub use decision::{DecisionCandidateV1, DecisionReceiptV1, ObservationRecordV1};
pub use delta::{
    DeltaReconciliationV1, ExpectedHeadSetV1, HeadConflictV1, HeadCoordinateV1, InformationDeltaV1,
    ReconciliationDispositionV1,
};
pub use exchange::{
    information_pack_key_id_v1, InformationDeltaPackV1, InformationPackKeyResolverV1,
    InformationPackSignerV1, OfflinePackPolicyV1, PackedInformationObjectV1,
    SignedInformationDeltaPackV1, TypedInformationObjectV1, VerifiedInformationDeltaPackV1,
    MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1, MAX_OFFLINE_INFORMATION_PACK_BODY_BYTES_V1,
    MAX_OFFLINE_INFORMATION_PACK_OBJECTS_V1, MAX_SIGNED_INFORMATION_PACK_BYTES_V1,
};
pub use id::{
    AtomIdV1, BlobIdV1, DecisionIdV1, DeltaIdV1, EntityIdV1, ObservationIdV1,
    ProjectionReceiptIdV1, RevisionIdV1, SnapshotRootIdV1,
};
pub use invalidation::ReceiptDependencyIndexV1;
pub use loss::{LossContractV1, LossKindV1, ProjectionDispositionV1};
pub use model::{
    AcquisitionModalityV1, EntityDescriptorV1, ExternalPayloadRefV1, InformationAtomV1,
    ManagedPayloadRefV1, NativeRecordRefV1, ParticipantV1, PayloadRefV1, PublicScalarV1, ScopeV1,
};
pub use projection::{ProjectionDirectionV1, ProjectionReceiptV1};
pub use root::{InformationRevisionV1, InformationSnapshotV1};
pub use store::{InformationObjectKindV1, InformationStoreV1};

pub const INFORMATION_SCHEMA_V1: &str = "ostadix.information/v1";
pub const MAX_T0_CANONICAL_BYTES: usize = 4 * 1024;
pub const MAX_T1_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InformationErrorV1 {
    #[error("invalid {kind} sha256 digest: {value}")]
    InvalidDigest { kind: &'static str, value: String },
    #[error("information canonical encoding failed: {0}")]
    Canonical(String),
    #[error("invalid information record: {0}")]
    InvalidRecord(String),
    #[error("forbidden secret- or authority-bearing payload schema: {0}")]
    ForbiddenPayload(String),
    #[error("T0 payload is {actual} canonical bytes; maximum is {maximum}")]
    T0TooLarge { actual: usize, maximum: usize },
    #[error("T1 payload is {actual} bytes; maximum is {maximum}")]
    T1TooLarge { actual: u64, maximum: u64 },
    #[error("projection loss contract has definite losses absent from possible losses")]
    InvalidLossContract,
    #[error("information revision may contain at most two distinct parents")]
    TooManyParents,
    #[error("information store I/O failure: {0}")]
    Io(String),
    #[error("information store root is already locked: {0}")]
    StoreLocked(String),
    #[error("information object digest mismatch: expected {expected}, actual {actual}")]
    ObjectDigestMismatch { expected: String, actual: String },
    #[error("information head {name} changed: expected {expected:?}, observed {observed:?}")]
    HeadConflict {
        name: String,
        expected: Option<String>,
        observed: Option<String>,
    },
    #[error("information signature failure: {0}")]
    Signature(String),
    #[error("untrusted information pack signer: {0}")]
    UntrustedSigner(String),
}

pub(crate) fn canonical_bytes<T: serde::Serialize>(
    value: &T,
) -> Result<Vec<u8>, InformationErrorV1> {
    crate::canonical_cbor::encode(value)
        .map_err(|error| InformationErrorV1::Canonical(error.to_string()))
}
