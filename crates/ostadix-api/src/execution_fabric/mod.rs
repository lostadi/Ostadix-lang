//! Canonical, authority-free records for the OIR execution-fabric boundary.
//!
//! V1 is deliberately a loopback protocol foundation.  Capsules describe one
//! source-closed trusted inline renderer and candidates remain provisional.
//! This module performs no placement, transport, execution, retry, graph
//! publication, or settlement.

mod codec;
mod protocol;

pub use codec::{
    decode_execution_candidate_v1, decode_execution_capsule_v1, encode_execution_candidate_v1,
    encode_execution_capsule_v1, execution_capsule_sha256_v1,
};
pub use protocol::{
    AttemptIdV1, CandidateOutcomeV1, CandidateOutputV1, ExecutionCandidateV1, ExecutionCapsuleV1,
    ExecutionFabricError, ExecutionFabricFailureClassV1, ExecutionIdV1, ExecutionLimitsV1,
    InputBindingV1, InputManifestV1, LogicalTaskIdV1, OutputContractV1, OutputFidelityV1,
    OutputValueKindV1, PortableValueV1, RendererPartV1, Sha256DigestV1, SourceClosedRendererV1,
    TrustedInlineRendererV1, EXECUTION_CANDIDATE_SCHEMA_V1, EXECUTION_CAPSULE_SCHEMA_V1,
    MAX_EXECUTION_CANDIDATE_BYTES, MAX_EXECUTION_CAPSULE_BYTES,
};
