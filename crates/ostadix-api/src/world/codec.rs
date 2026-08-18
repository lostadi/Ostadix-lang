//! Strict canonical records for World control-plane data.
//!
//! `OWPROTO` frames are deterministic, architecture-independent, and bounded.
//! They are records only: this module supplies no stream transport, peer
//! authentication, replay protection, session state, or authority.

use thiserror::Error;

use super::identity_wire::{
    identity_v1_conformance_records, IdentityWireError, IdentityWireRecord,
};
use super::protocol::{
    negotiate_schema, NegotiatedSchema, SchemaNegotiation, SchemaOffer, SchemaRejection,
    SchemaSelection, WorldProtocolError, MAX_WORLD_WIRE_RECORD_BYTES, WORLD_SCHEMA_V1,
};

pub const WORLD_WIRE_MAGIC: &[u8; 8] = b"OWPROTO\0";
pub const WORLD_WIRE_CODEC_VERSION: u16 = 1;
pub const WORLD_WIRE_HEADER_BYTES: usize = 16;

const NEGOTIATION_PAYLOAD_BYTES: usize = 8;
const IDENTITY_PREFIX_BYTES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum WorldWireKind {
    SchemaOffer = 0x0001,
    SchemaSelection = 0x0002,
    SchemaRejection = 0x0003,
    IdentityDescription = 0x0010,
}

impl WorldWireKind {
    fn from_u16(value: u16) -> Result<Self, WorldCodecError> {
        match value {
            0x0001 => Ok(Self::SchemaOffer),
            0x0002 => Ok(Self::SchemaSelection),
            0x0003 => Ok(Self::SchemaRejection),
            0x0010 => Ok(Self::IdentityDescription),
            found => Err(WorldCodecError::UnknownKind { found }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldWireRecord {
    SchemaOffer(SchemaOffer),
    /// A syntactically valid peer statement. It becomes an admitted
    /// [`NegotiatedSchema`] only through contextual `validate_selection`.
    SchemaSelection(SchemaSelection),
    SchemaRejection(SchemaRejection),
    /// Descriptive identity data only. In particular, a serialized
    /// `CapabilityId` is not a bearer, handle, or delegation.
    IdentityDescription(IdentityWireRecord),
}

impl WorldWireRecord {
    pub fn kind(&self) -> WorldWireKind {
        match self {
            Self::SchemaOffer(_) => WorldWireKind::SchemaOffer,
            Self::SchemaSelection(_) => WorldWireKind::SchemaSelection,
            Self::SchemaRejection(_) => WorldWireKind::SchemaRejection,
            Self::IdentityDescription(_) => WorldWireKind::IdentityDescription,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, WorldCodecError> {
        self.encode_with_limit(MAX_WORLD_WIRE_RECORD_BYTES)
    }

    pub fn encode_with_limit(&self, max_record_bytes: u32) -> Result<Vec<u8>, WorldCodecError> {
        let mut payload = Vec::new();
        match self {
            Self::SchemaOffer(offer) => {
                put_u16(&mut payload, offer.min_version());
                put_u16(&mut payload, offer.max_version());
                put_u32(&mut payload, offer.max_record_bytes());
            }
            Self::SchemaSelection(selection) => {
                put_u16(&mut payload, selection.version());
                put_u16(&mut payload, 0);
                put_u32(&mut payload, selection.max_record_bytes());
            }
            Self::SchemaRejection(rejection) => {
                put_u16(&mut payload, rejection.local_min());
                put_u16(&mut payload, rejection.local_max());
                put_u16(&mut payload, rejection.peer_min());
                put_u16(&mut payload, rejection.peer_max());
            }
            Self::IdentityDescription(identity) => {
                put_u16(&mut payload, WORLD_SCHEMA_V1);
                put_u16(&mut payload, 0);
                payload.extend_from_slice(&identity.encode()?);
            }
        }

        let total_len = WORLD_WIRE_HEADER_BYTES.checked_add(payload.len()).ok_or(
            WorldCodecError::RecordTooLarge {
                actual: usize::MAX,
                maximum: MAX_WORLD_WIRE_RECORD_BYTES as usize,
            },
        )?;
        enforce_limit(total_len, max_record_bytes)?;
        let total_len = u32::try_from(total_len).map_err(|_| WorldCodecError::RecordTooLarge {
            actual: total_len,
            maximum: MAX_WORLD_WIRE_RECORD_BYTES as usize,
        })?;

        let mut record = Vec::with_capacity(total_len as usize);
        record.extend_from_slice(WORLD_WIRE_MAGIC);
        put_u16(&mut record, WORLD_WIRE_CODEC_VERSION);
        put_u16(&mut record, self.kind() as u16);
        put_u32(&mut record, total_len);
        record.extend_from_slice(&payload);
        Ok(record)
    }

    pub fn decode(record: &[u8]) -> Result<Self, WorldCodecError> {
        Self::decode_with_limit(record, MAX_WORLD_WIRE_RECORD_BYTES)
    }

    pub fn decode_with_limit(
        record: &[u8],
        max_record_bytes: u32,
    ) -> Result<Self, WorldCodecError> {
        enforce_limit(record.len(), max_record_bytes)?;
        if record.len() < WORLD_WIRE_HEADER_BYTES {
            return Err(WorldCodecError::Truncated {
                needed: WORLD_WIRE_HEADER_BYTES,
                remaining: record.len(),
            });
        }
        if &record[..WORLD_WIRE_MAGIC.len()] != WORLD_WIRE_MAGIC {
            return Err(WorldCodecError::BadMagic);
        }
        let codec_version = read_u16(record, 8);
        if codec_version != WORLD_WIRE_CODEC_VERSION {
            return Err(WorldCodecError::UnsupportedCodecVersion {
                found: codec_version,
            });
        }
        let kind = WorldWireKind::from_u16(read_u16(record, 10))?;
        let declared = read_u32(record, 12) as usize;
        enforce_limit(declared, max_record_bytes)?;
        if declared != record.len() {
            return Err(WorldCodecError::LengthMismatch {
                declared,
                actual: record.len(),
            });
        }

        let payload = &record[WORLD_WIRE_HEADER_BYTES..];
        match kind {
            WorldWireKind::SchemaOffer => {
                require_payload_len(kind, payload, NEGOTIATION_PAYLOAD_BYTES)?;
                Ok(Self::SchemaOffer(SchemaOffer::new(
                    read_u16(payload, 0),
                    read_u16(payload, 2),
                    read_u32(payload, 4),
                )?))
            }
            WorldWireKind::SchemaSelection => {
                require_payload_len(kind, payload, NEGOTIATION_PAYLOAD_BYTES)?;
                let reserved = read_u16(payload, 2);
                if reserved != 0 {
                    return Err(WorldCodecError::NonzeroReserved {
                        kind,
                        found: reserved,
                    });
                }
                Ok(Self::SchemaSelection(SchemaSelection::new(
                    read_u16(payload, 0),
                    read_u32(payload, 4),
                )?))
            }
            WorldWireKind::SchemaRejection => {
                require_payload_len(kind, payload, NEGOTIATION_PAYLOAD_BYTES)?;
                Ok(Self::SchemaRejection(SchemaRejection::no_common_version(
                    read_u16(payload, 0),
                    read_u16(payload, 2),
                    read_u16(payload, 4),
                    read_u16(payload, 6),
                )?))
            }
            WorldWireKind::IdentityDescription => {
                if payload.len() < IDENTITY_PREFIX_BYTES {
                    return Err(WorldCodecError::Truncated {
                        needed: IDENTITY_PREFIX_BYTES,
                        remaining: payload.len(),
                    });
                }
                let schema = read_u16(payload, 0);
                if schema != WORLD_SCHEMA_V1 {
                    return Err(WorldCodecError::UnsupportedPayloadSchema {
                        kind,
                        found: schema,
                    });
                }
                let reserved = read_u16(payload, 2);
                if reserved != 0 {
                    return Err(WorldCodecError::NonzeroReserved {
                        kind,
                        found: reserved,
                    });
                }
                let nested = &payload[IDENTITY_PREFIX_BYTES..];
                let identity = IdentityWireRecord::decode(nested)?;
                if identity.encode()?.as_slice() != nested {
                    return Err(WorldCodecError::NonCanonicalIdentity);
                }
                Ok(Self::IdentityDescription(identity))
            }
        }
    }

    /// Decode one post-negotiation identity record while enforcing both the
    /// admitted schema and its agreed record bound.
    pub fn decode_identity_with_negotiated_schema(
        record: &[u8],
        negotiated: NegotiatedSchema,
    ) -> Result<IdentityWireRecord, WorldCodecError> {
        if negotiated.version() != WORLD_SCHEMA_V1 {
            return Err(WorldCodecError::UnsupportedNegotiatedSchema {
                found: negotiated.version(),
            });
        }
        match Self::decode_with_limit(record, negotiated.max_record_bytes())? {
            Self::IdentityDescription(identity) => Ok(identity),
            other => Err(WorldCodecError::ExpectedIdentityDescription {
                found: other.kind(),
            }),
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorldCodecError {
    #[error("World record is {actual} bytes; protocol maximum is {maximum}")]
    RecordTooLarge { actual: usize, maximum: usize },
    #[error("World record is {actual} bytes; negotiated/caller limit is {limit}")]
    RecordExceedsLimit { actual: usize, limit: u32 },
    #[error("World record is truncated: need {needed} bytes, have {remaining}")]
    Truncated { needed: usize, remaining: usize },
    #[error("World record has invalid magic")]
    BadMagic,
    #[error("unsupported World codec version {found}")]
    UnsupportedCodecVersion { found: u16 },
    #[error("unknown World wire kind {found}")]
    UnknownKind { found: u16 },
    #[error("World record length says {declared} bytes but input has {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("World {kind:?} payload is {actual} bytes; canonical length is {expected}")]
    InvalidPayloadLength {
        kind: WorldWireKind,
        expected: usize,
        actual: usize,
    },
    #[error("World {kind:?} reserved field is nonzero: {found}")]
    NonzeroReserved { kind: WorldWireKind, found: u16 },
    #[error("World {kind:?} payload schema {found} is unsupported")]
    UnsupportedPayloadSchema { kind: WorldWireKind, found: u16 },
    #[error("negotiated World schema {found} is unsupported by this data codec")]
    UnsupportedNegotiatedSchema { found: u16 },
    #[error("expected a post-negotiation identity description, found {found:?}")]
    ExpectedIdentityDescription { found: WorldWireKind },
    #[error("nested identity has a noncanonical byte representation")]
    NonCanonicalIdentity,
    #[error(transparent)]
    Protocol(#[from] WorldProtocolError),
    #[error(transparent)]
    Identity(#[from] IdentityWireError),
}

fn enforce_limit(actual: usize, caller_limit: u32) -> Result<(), WorldCodecError> {
    if actual > MAX_WORLD_WIRE_RECORD_BYTES as usize {
        return Err(WorldCodecError::RecordTooLarge {
            actual,
            maximum: MAX_WORLD_WIRE_RECORD_BYTES as usize,
        });
    }
    if actual > caller_limit as usize {
        return Err(WorldCodecError::RecordExceedsLimit {
            actual,
            limit: caller_limit,
        });
    }
    Ok(())
}

fn require_payload_len(
    kind: WorldWireKind,
    payload: &[u8],
    expected: usize,
) -> Result<(), WorldCodecError> {
    if payload.len() != expected {
        return Err(WorldCodecError::InvalidPayloadLength {
            kind,
            expected,
            actual: payload.len(),
        });
    }
    Ok(())
}

fn put_u16(target: &mut Vec<u8>, value: u16) {
    target.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(target: &mut Vec<u8>, value: u32) {
    target.extend_from_slice(&value.to_be_bytes());
}

fn read_u16(source: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([source[offset], source[offset + 1]])
}

fn read_u32(source: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        source[offset],
        source[offset + 1],
        source[offset + 2],
        source[offset + 3],
    ])
}

/// Fixed cross-implementation corpus for version-1 framing and payloads.
///
/// The order and literal values are part of the conformance oracle. Identity
/// descriptions intentionally include every identity-v1 conformance record.
pub fn world_protocol_v1_conformance_records() -> Vec<WorldWireRecord> {
    let local = SchemaOffer::v1(MAX_WORLD_WIRE_RECORD_BYTES)
        .expect("conformance local schema offer is valid");
    let peer = SchemaOffer::new(1, 3, 1024).expect("conformance peer schema offer is valid");
    let selected = match negotiate_schema(local, peer) {
        SchemaNegotiation::Selected(selected) => selected.selection(),
        SchemaNegotiation::Rejected(_) => panic!("conformance offers must overlap"),
    };
    let mut records = vec![
        WorldWireRecord::SchemaOffer(local),
        WorldWireRecord::SchemaOffer(peer),
        WorldWireRecord::SchemaSelection(selected),
        WorldWireRecord::SchemaRejection(
            SchemaRejection::no_common_version(1, 1, 2, 3)
                .expect("conformance schema rejection is valid"),
        ),
    ];
    records.extend(
        identity_v1_conformance_records()
            .into_iter()
            .map(WorldWireRecord::IdentityDescription),
    );
    records
}

/// Concatenate the fixed records without adding a stream transport envelope.
pub fn world_protocol_v1_conformance_bytes() -> Result<Vec<u8>, WorldCodecError> {
    let mut bytes = Vec::new();
    for record in world_protocol_v1_conformance_records() {
        bytes.extend_from_slice(&record.encode()?);
    }
    Ok(bytes)
}
