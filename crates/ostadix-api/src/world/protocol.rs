//! Architecture-independent control vocabulary for the canonical World wire
//! codec.
//!
//! Version negotiation is a deterministic, offline calculation over two
//! bounded offers. It does not open a transport, authenticate a peer, or grant
//! authority.

use thiserror::Error;

pub const WORLD_SCHEMA_V1: u16 = 1;
pub const WORLD_WIRE_MIN_RECORD_BYTES: u32 = 24;
pub const MAX_WORLD_WIRE_RECORD_BYTES: u32 = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaOffer {
    min_version: u16,
    max_version: u16,
    max_record_bytes: u32,
}

impl SchemaOffer {
    /// Construct an inspected offer. Ranges above v1 can describe a future
    /// peer, but this implementation must advertise [`Self::v1`] for its own
    /// supported data schema.
    pub fn new(
        min_version: u16,
        max_version: u16,
        max_record_bytes: u32,
    ) -> Result<Self, WorldProtocolError> {
        validate_range(min_version, max_version)?;
        validate_record_limit(max_record_bytes)?;
        Ok(Self {
            min_version,
            max_version,
            max_record_bytes,
        })
    }

    pub fn v1(max_record_bytes: u32) -> Result<Self, WorldProtocolError> {
        Self::new(WORLD_SCHEMA_V1, WORLD_SCHEMA_V1, max_record_bytes)
    }

    pub fn min_version(self) -> u16 {
        self.min_version
    }

    pub fn max_version(self) -> u16 {
        self.max_version
    }

    pub fn max_record_bytes(self) -> u32 {
        self.max_record_bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaSelection {
    version: u16,
    max_record_bytes: u32,
}

impl SchemaSelection {
    pub fn new(version: u16, max_record_bytes: u32) -> Result<Self, WorldProtocolError> {
        if version == 0 {
            return Err(WorldProtocolError::ZeroSchemaVersion);
        }
        validate_record_limit(max_record_bytes)?;
        Ok(Self {
            version,
            max_record_bytes,
        })
    }

    pub fn version(self) -> u16 {
        self.version
    }

    pub fn max_record_bytes(self) -> u32 {
        self.max_record_bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum SchemaRejectionReason {
    /// Implicit in the `SchemaRejection` wire kind. Version 1 has no encoded
    /// free-form reason field, so future reasons require a new schema.
    NoCommonVersion = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaRejection {
    reason: SchemaRejectionReason,
    local_min: u16,
    local_max: u16,
    peer_min: u16,
    peer_max: u16,
}

impl SchemaRejection {
    pub fn no_common_version(
        local_min: u16,
        local_max: u16,
        peer_min: u16,
        peer_max: u16,
    ) -> Result<Self, WorldProtocolError> {
        validate_range(local_min, local_max)?;
        validate_range(peer_min, peer_max)?;
        if ranges_overlap(local_min, local_max, peer_min, peer_max) {
            return Err(WorldProtocolError::NonCanonicalRejection);
        }
        Ok(Self {
            reason: SchemaRejectionReason::NoCommonVersion,
            local_min,
            local_max,
            peer_min,
            peer_max,
        })
    }

    pub fn reason(self) -> SchemaRejectionReason {
        self.reason
    }

    pub fn local_min(self) -> u16 {
        self.local_min
    }

    pub fn local_max(self) -> u16 {
        self.local_max
    }

    pub fn peer_min(self) -> u16 {
        self.peer_min
    }

    pub fn peer_max(self) -> u16 {
        self.peer_max
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaNegotiation {
    Selected(NegotiatedSchema),
    Rejected(SchemaRejection),
}

/// A canonical selection derived from two validated offers.
///
/// Its fields are private so a wire-decoded `SchemaSelection` remains an
/// unadmitted peer statement until [`validate_selection`] checks its context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NegotiatedSchema {
    selection: SchemaSelection,
}

impl NegotiatedSchema {
    pub fn version(self) -> u16 {
        self.selection.version
    }

    pub fn max_record_bytes(self) -> u32 {
        self.selection.max_record_bytes
    }

    pub fn selection(self) -> SchemaSelection {
        self.selection
    }
}

/// Select the highest common version and the smaller receive bound.
///
/// The result depends only on the two validated offers. Choosing the highest
/// common version makes downgrades and alternative encodings noncanonical.
pub fn negotiate_schema(local: SchemaOffer, peer: SchemaOffer) -> SchemaNegotiation {
    let lower = local.min_version.max(peer.min_version);
    let upper = local.max_version.min(peer.max_version);
    if lower <= upper {
        return SchemaNegotiation::Selected(NegotiatedSchema {
            selection: SchemaSelection {
                version: upper,
                max_record_bytes: local.max_record_bytes.min(peer.max_record_bytes),
            },
        });
    }
    SchemaNegotiation::Rejected(SchemaRejection {
        reason: SchemaRejectionReason::NoCommonVersion,
        local_min: local.min_version,
        local_max: local.max_version,
        peer_min: peer.min_version,
        peer_max: peer.max_version,
    })
}

/// Admit a peer selection only when it is exactly the canonical result for the
/// two offers. This rejects both version downgrades and inflated frame limits.
pub fn validate_selection(
    local: SchemaOffer,
    peer: SchemaOffer,
    selected: SchemaSelection,
) -> Result<NegotiatedSchema, WorldProtocolError> {
    match negotiate_schema(local, peer) {
        SchemaNegotiation::Selected(expected) if expected.selection == selected => Ok(expected),
        SchemaNegotiation::Selected(expected) => Err(WorldProtocolError::NonCanonicalSelection {
            expected_version: expected.version(),
            expected_max_record_bytes: expected.max_record_bytes(),
            found_version: selected.version,
            found_max_record_bytes: selected.max_record_bytes,
        }),
        SchemaNegotiation::Rejected(_) => Err(WorldProtocolError::SelectionWithoutOverlap),
    }
}

/// Admit a rejection only when it is exactly the canonical no-overlap result
/// for the two offers. The range fields are therefore evidence of the inputs,
/// not free-form peer assertions.
pub fn validate_rejection(
    local: SchemaOffer,
    peer: SchemaOffer,
    rejected: SchemaRejection,
) -> Result<(), WorldProtocolError> {
    match negotiate_schema(local, peer) {
        SchemaNegotiation::Rejected(expected) if expected == rejected => Ok(()),
        SchemaNegotiation::Rejected(_) => Err(WorldProtocolError::NonCanonicalRejectionForOffers),
        SchemaNegotiation::Selected(_) => Err(WorldProtocolError::RejectionWithOverlap),
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorldProtocolError {
    #[error("schema version zero is reserved for negotiation control frames")]
    ZeroSchemaVersion,
    #[error("schema range {min}..={max} is inverted")]
    InvertedSchemaRange { min: u16, max: u16 },
    #[error("maximum record size {found} is below the protocol minimum {minimum}")]
    RecordLimitTooSmall { found: u32, minimum: u32 },
    #[error("maximum record size {found} exceeds the protocol maximum {maximum}")]
    RecordLimitTooLarge { found: u32, maximum: u32 },
    #[error("schema rejection is noncanonical because the offered ranges overlap")]
    NonCanonicalRejection,
    #[error("schema rejection does not reproduce the two original offers")]
    NonCanonicalRejectionForOffers,
    #[error("a schema was rejected even though the offers overlap")]
    RejectionWithOverlap,
    #[error(
        "noncanonical schema selection: expected version {expected_version} with {expected_max_record_bytes} bytes, found version {found_version} with {found_max_record_bytes} bytes"
    )]
    NonCanonicalSelection {
        expected_version: u16,
        expected_max_record_bytes: u32,
        found_version: u16,
        found_max_record_bytes: u32,
    },
    #[error("a schema was selected even though the offers do not overlap")]
    SelectionWithoutOverlap,
}

fn validate_range(min: u16, max: u16) -> Result<(), WorldProtocolError> {
    if min == 0 || max == 0 {
        return Err(WorldProtocolError::ZeroSchemaVersion);
    }
    if min > max {
        return Err(WorldProtocolError::InvertedSchemaRange { min, max });
    }
    Ok(())
}

fn validate_record_limit(max_record_bytes: u32) -> Result<(), WorldProtocolError> {
    if max_record_bytes < WORLD_WIRE_MIN_RECORD_BYTES {
        return Err(WorldProtocolError::RecordLimitTooSmall {
            found: max_record_bytes,
            minimum: WORLD_WIRE_MIN_RECORD_BYTES,
        });
    }
    if max_record_bytes > MAX_WORLD_WIRE_RECORD_BYTES {
        return Err(WorldProtocolError::RecordLimitTooLarge {
            found: max_record_bytes,
            maximum: MAX_WORLD_WIRE_RECORD_BYTES,
        });
    }
    Ok(())
}

fn ranges_overlap(local_min: u16, local_max: u16, peer_min: u16, peer_max: u16) -> bool {
    local_min.max(peer_min) <= local_max.min(peer_max)
}
