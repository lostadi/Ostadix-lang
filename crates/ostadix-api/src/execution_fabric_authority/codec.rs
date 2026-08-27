use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::canonical_cbor::{decode_bounded, encode, DecodeLimits};
use crate::execution_fabric::{MAX_EXECUTION_CANDIDATE_BYTES, MAX_EXECUTION_CAPSULE_BYTES};

use super::protocol::{
    domain_sha256, FabricAbandonmentV1, FabricAttemptQueryV1, FabricAttemptStatusV1,
    FabricAuthorityError, FabricRejectionV1, FabricRequestV1, FabricResponseV1,
    FabricSubmissionHeaderV1, FabricSubmissionV1, FabricTerminalCandidateV1, PlacementLeaseV3,
    SignedTerminalCandidateReceiptV1, FABRIC_REQUEST_SCHEMA_V1, FABRIC_RESPONSE_SCHEMA_V1,
    MAX_FABRIC_HEADER_BYTES,
};

const MAX_FABRIC_HEADER_ITEMS: usize = 200_000;
const MAX_FABRIC_HEADER_DEPTH: usize = 64;
const FABRIC_LEASE_DIGEST_DOMAIN_V3: &[u8] = b"ostadix/execution-fabric/lease/v3";

/// One canonical header plus an optional exact opaque M2 payload.
///
/// The transport length-prefixes these byte vectors independently. Keeping
/// the payload outside serde prevents an alternate CBOR integer-array encoding
/// of capsule/candidate bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FabricEncodedMessageV1 {
    header_bytes: Vec<u8>,
    payload_bytes: Option<Vec<u8>>,
}

impl FabricEncodedMessageV1 {
    pub fn header_bytes(&self) -> &[u8] {
        &self.header_bytes
    }

    pub fn payload_bytes(&self) -> Option<&[u8]> {
        self.payload_bytes.as_deref()
    }

    pub fn into_parts(self) -> (Vec<u8>, Option<Vec<u8>>) {
        (self.header_bytes, self.payload_bytes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", content = "body", rename_all = "kebab-case")]
enum FabricRequestHeaderKindV1 {
    SubmitPureAttempt(FabricSubmissionHeaderV1),
    QueryAttempt(FabricAttemptQueryV1),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FabricRequestEnvelopeV1 {
    schema: String,
    request: FabricRequestHeaderKindV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "body", rename_all = "kebab-case")]
enum FabricResponseHeaderKindV1 {
    Accepted(FabricAttemptStatusV1),
    Running(FabricAttemptStatusV1),
    TerminalCandidate(SignedTerminalCandidateReceiptV1),
    Rejected(FabricRejectionV1),
    Abandoned(FabricAbandonmentV1),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FabricResponseEnvelopeV1 {
    schema: String,
    response: FabricResponseHeaderKindV1,
}

pub fn encode_placement_lease_v3(
    lease: &PlacementLeaseV3,
) -> Result<Vec<u8>, FabricAuthorityError> {
    lease.validate()?;
    encode_canonical_bounded("placement lease v3", lease, MAX_FABRIC_HEADER_BYTES)
}

pub fn fabric_lease_sha256_v3(lease: &PlacementLeaseV3) -> Result<[u8; 32], FabricAuthorityError> {
    Ok(domain_sha256(
        FABRIC_LEASE_DIGEST_DOMAIN_V3,
        &encode_placement_lease_v3(lease)?,
    ))
}

pub fn encode_fabric_request_v1(
    request: &FabricRequestV1,
) -> Result<FabricEncodedMessageV1, FabricAuthorityError> {
    let (request, payload_bytes) = match request {
        FabricRequestV1::SubmitPureAttempt(submission) => {
            submission.validate()?;
            (
                FabricRequestHeaderKindV1::SubmitPureAttempt(submission.header().clone()),
                Some(submission.capsule_bytes().to_vec()),
            )
        }
        FabricRequestV1::QueryAttempt(query) => {
            query.validate()?;
            (FabricRequestHeaderKindV1::QueryAttempt(query.clone()), None)
        }
    };
    let envelope = FabricRequestEnvelopeV1 {
        schema: FABRIC_REQUEST_SCHEMA_V1.to_string(),
        request,
    };
    Ok(FabricEncodedMessageV1 {
        header_bytes: encode_canonical_bounded(
            "request header",
            &envelope,
            MAX_FABRIC_HEADER_BYTES,
        )?,
        payload_bytes,
    })
}

pub fn decode_fabric_request_v1(
    header_bytes: &[u8],
    payload_bytes: Option<&[u8]>,
) -> Result<FabricRequestV1, FabricAuthorityError> {
    let envelope: FabricRequestEnvelopeV1 =
        decode_canonical_bounded("request header", header_bytes, MAX_FABRIC_HEADER_BYTES)?;
    if envelope.schema != FABRIC_REQUEST_SCHEMA_V1 {
        return Err(super::protocol::invalid(
            "unsupported Fabric request envelope schema",
        ));
    }
    match envelope.request {
        FabricRequestHeaderKindV1::SubmitPureAttempt(header) => {
            let payload = payload_bytes.ok_or_else(|| {
                super::protocol::invalid("submit-pure-attempt omitted its capsule payload")
            })?;
            if payload.len() > MAX_EXECUTION_CAPSULE_BYTES {
                return Err(FabricAuthorityError::RecordTooLarge {
                    kind: "capsule",
                    actual: payload.len(),
                    maximum: MAX_EXECUTION_CAPSULE_BYTES,
                });
            }
            Ok(FabricRequestV1::SubmitPureAttempt(
                FabricSubmissionV1::from_wire(header, payload.to_vec())?,
            ))
        }
        FabricRequestHeaderKindV1::QueryAttempt(query) => {
            reject_unexpected_payload(payload_bytes, "query-attempt")?;
            query.validate()?;
            Ok(FabricRequestV1::QueryAttempt(query))
        }
    }
}

pub fn encode_fabric_response_v1(
    response: &FabricResponseV1,
) -> Result<FabricEncodedMessageV1, FabricAuthorityError> {
    let (response, payload_bytes) = match response {
        FabricResponseV1::Accepted(status) => {
            status.validate()?;
            (FabricResponseHeaderKindV1::Accepted(status.clone()), None)
        }
        FabricResponseV1::Running(status) => {
            status.validate()?;
            (FabricResponseHeaderKindV1::Running(status.clone()), None)
        }
        FabricResponseV1::TerminalCandidate(terminal) => {
            terminal.validate()?;
            (
                FabricResponseHeaderKindV1::TerminalCandidate(terminal.signed_receipt().clone()),
                Some(terminal.candidate_bytes().to_vec()),
            )
        }
        FabricResponseV1::Rejected(rejection) => {
            rejection.validate()?;
            (
                FabricResponseHeaderKindV1::Rejected(rejection.clone()),
                None,
            )
        }
        FabricResponseV1::Abandoned(abandonment) => {
            abandonment.validate()?;
            (
                FabricResponseHeaderKindV1::Abandoned(abandonment.clone()),
                None,
            )
        }
    };
    let envelope = FabricResponseEnvelopeV1 {
        schema: FABRIC_RESPONSE_SCHEMA_V1.to_string(),
        response,
    };
    Ok(FabricEncodedMessageV1 {
        header_bytes: encode_canonical_bounded(
            "response header",
            &envelope,
            MAX_FABRIC_HEADER_BYTES,
        )?,
        payload_bytes,
    })
}

pub fn decode_fabric_response_v1(
    header_bytes: &[u8],
    payload_bytes: Option<&[u8]>,
) -> Result<FabricResponseV1, FabricAuthorityError> {
    let envelope: FabricResponseEnvelopeV1 =
        decode_canonical_bounded("response header", header_bytes, MAX_FABRIC_HEADER_BYTES)?;
    if envelope.schema != FABRIC_RESPONSE_SCHEMA_V1 {
        return Err(super::protocol::invalid(
            "unsupported Fabric response envelope schema",
        ));
    }
    match envelope.response {
        FabricResponseHeaderKindV1::Accepted(status) => {
            reject_unexpected_payload(payload_bytes, "accepted")?;
            status.validate()?;
            Ok(FabricResponseV1::Accepted(status))
        }
        FabricResponseHeaderKindV1::Running(status) => {
            reject_unexpected_payload(payload_bytes, "running")?;
            status.validate()?;
            Ok(FabricResponseV1::Running(status))
        }
        FabricResponseHeaderKindV1::TerminalCandidate(receipt) => {
            let payload = payload_bytes.ok_or_else(|| {
                super::protocol::invalid("terminal-candidate omitted its candidate payload")
            })?;
            if payload.len() > MAX_EXECUTION_CANDIDATE_BYTES {
                return Err(FabricAuthorityError::RecordTooLarge {
                    kind: "candidate",
                    actual: payload.len(),
                    maximum: MAX_EXECUTION_CANDIDATE_BYTES,
                });
            }
            Ok(FabricResponseV1::TerminalCandidate(
                FabricTerminalCandidateV1::from_wire(receipt, payload.to_vec())?,
            ))
        }
        FabricResponseHeaderKindV1::Rejected(rejection) => {
            reject_unexpected_payload(payload_bytes, "rejected")?;
            rejection.validate()?;
            Ok(FabricResponseV1::Rejected(rejection))
        }
        FabricResponseHeaderKindV1::Abandoned(abandonment) => {
            reject_unexpected_payload(payload_bytes, "abandoned")?;
            abandonment.validate()?;
            Ok(FabricResponseV1::Abandoned(abandonment))
        }
    }
}

fn reject_unexpected_payload(
    payload_bytes: Option<&[u8]>,
    kind: &str,
) -> Result<(), FabricAuthorityError> {
    if payload_bytes.is_some() {
        return Err(super::protocol::invalid(format!(
            "Fabric {kind} message carried an unexpected payload"
        )));
    }
    Ok(())
}

fn encode_canonical_bounded<T: Serialize>(
    kind: &'static str,
    value: &T,
    maximum: usize,
) -> Result<Vec<u8>, FabricAuthorityError> {
    let bytes = encode(value).map_err(|error| {
        FabricAuthorityError::Codec(format!("failed to encode {kind}: {error:#}"))
    })?;
    if bytes.len() > maximum {
        return Err(FabricAuthorityError::RecordTooLarge {
            kind,
            actual: bytes.len(),
            maximum,
        });
    }
    Ok(bytes)
}

fn decode_canonical_bounded<T: DeserializeOwned + Serialize>(
    kind: &'static str,
    bytes: &[u8],
    maximum: usize,
) -> Result<T, FabricAuthorityError> {
    if bytes.len() > maximum {
        return Err(FabricAuthorityError::RecordTooLarge {
            kind,
            actual: bytes.len(),
            maximum,
        });
    }
    let value = decode_bounded(
        bytes,
        DecodeLimits {
            max_bytes: maximum,
            max_items: MAX_FABRIC_HEADER_ITEMS,
            max_depth: MAX_FABRIC_HEADER_DEPTH,
        },
    )
    .map_err(|error| FabricAuthorityError::Codec(format!("failed to decode {kind}: {error:#}")))?;
    let canonical = encode(&value).map_err(|error| {
        FabricAuthorityError::Codec(format!("failed to re-encode {kind}: {error:#}"))
    })?;
    if canonical != bytes {
        return Err(FabricAuthorityError::NonCanonical { kind });
    }
    Ok(value)
}
