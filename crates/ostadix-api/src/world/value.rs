//! Portable, authority-free values for the Ostadix World boundary.
//!
//! This is deliberately distinct from [`crate::value::OValue`]. Hosted
//! `OValue` includes live references, capabilities, executable requests, and
//! backend capsules. Only an explicit structural allowlist can enter this
//! module, and decoding these values never resolves names or bytes into
//! authority.

use num_bigint::BigInt;
use thiserror::Error;

use crate::value::{FloatFormat, OBytes, ONumber, OText, OValue};

use super::identity::ObjectIdentity;
use super::identity_wire::IdentityWireError;

/// Portable core values. Extensions are root envelopes in
/// [`PortableValueRecord`], not recursive core variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortableOValue {
    Null,
    Bool(bool),
    /// PR4 v1 admits the `Int` and `BinaryFloat` hosted forms without coercion.
    Number(ONumber),
    Text(OText),
    Char(char),
    Bytes(OBytes),
    List(Vec<PortableOValue>),
    /// Raw variants remain public so validation can prove that noncanonical
    /// order and duplicates fail. Use [`Self::record`] for normal construction.
    Record(Vec<(String, PortableOValue)>),
    /// Raw variants remain public so validation can prove that noncanonical
    /// order and duplicates fail. Use [`Self::map`] for normal construction.
    Map(Vec<(PortableOValue, PortableOValue)>),
    Tagged(PortableTagged),
    CodeRef(PortableCodeRef),
    /// Descriptive object identity only. Access still requires separate live
    /// authority and current-version validation.
    ObjectRef(ObjectIdentity),
    /// Inert failure data, distinct from the hosted effectful `OValue::Error`.
    Error(PortableError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableTagged {
    pub(super) tag: String,
    pub(super) value: Box<PortableOValue>,
}

impl PortableTagged {
    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn value(&self) -> &PortableOValue {
        &self.value
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableCodeRef {
    pub(super) digest: [u8; 32],
    pub(super) evaluator: String,
    pub(super) entrypoint: String,
}

impl PortableCodeRef {
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    pub fn evaluator(&self) -> &str {
        &self.evaluator
    }

    pub fn entrypoint(&self) -> &str {
        &self.entrypoint
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableError {
    pub(super) code: String,
    pub(super) message: String,
}

impl PortableError {
    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// One complete OWVALUE record is either a core value or one inert extension
/// envelope. Extensions cannot nest as core variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortableValueRecord {
    Core(PortableOValue),
    Extension(ExtensionEnvelope),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionEnvelope {
    pub(super) namespace: String,
    pub(super) name: String,
    pub(super) version: u16,
    pub(super) schema_digest: [u8; 32],
    pub(super) value: Box<PortableOValue>,
}

impl ExtensionEnvelope {
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> u16 {
        self.version
    }

    pub fn schema_digest(&self) -> &[u8; 32] {
        &self.schema_digest
    }

    pub fn value(&self) -> &PortableOValue {
        &self.value
    }

    /// Admit an extension only for one exact independently supplied schema.
    /// Merely decoding an envelope never performs this admission.
    pub fn admit_exact(
        &self,
        namespace: &str,
        name: &str,
        version: u16,
        schema_digest: &[u8; 32],
    ) -> Result<AdmittedExtension, PortableValueError> {
        if self.namespace != namespace
            || self.name != name
            || self.version != version
            || &self.schema_digest != schema_digest
        {
            return Err(PortableValueError::ExtensionSchemaMismatch);
        }
        Ok(AdmittedExtension {
            envelope: self.clone(),
        })
    }
}

/// An extension whose namespace, name, version, and schema digest were matched
/// exactly. Fields are private so raw wire input cannot manufacture admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedExtension {
    envelope: ExtensionEnvelope,
}

impl AdmittedExtension {
    pub fn envelope(&self) -> &ExtensionEnvelope {
        &self.envelope
    }

    pub fn value(&self) -> &PortableOValue {
        self.envelope.value()
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HostedValueError {
    #[error("hosted {kind} carries authority and is not a portable OValue")]
    AuthorityBearing { kind: &'static str },
    #[error("hosted {kind} is affinity-bound or referential and is not a portable OValue")]
    CapsuleBound { kind: &'static str },
    #[error("hosted {kind} carries execution/effect semantics and is not a portable OValue")]
    Effectful { kind: &'static str },
    #[error("hosted {kind} needs an explicit portable extension adapter")]
    Unsupported { kind: &'static str },
    #[error(transparent)]
    InvalidPortable(#[from] PortableValueError),
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PortableValueError {
    #[error("OWVALUE record is {actual} bytes; hard maximum is {maximum}")]
    RecordTooLarge { actual: usize, maximum: usize },
    #[error("OWVALUE record is {actual} bytes; minimum is {minimum}")]
    RecordTooSmall { actual: usize, minimum: usize },
    #[error("OWVALUE record is {actual} bytes; negotiated/caller limit is {limit}")]
    RecordExceedsLimit { actual: usize, limit: u32 },
    #[error("OWVALUE input is truncated: need {needed} bytes, have {remaining}")]
    Truncated { needed: usize, remaining: usize },
    #[error("OWVALUE record has invalid magic")]
    BadMagic,
    #[error("unsupported OWVALUE schema {found}")]
    UnsupportedSchema { found: u16 },
    #[error("negotiated World schema {found} cannot carry OWVALUE v1")]
    UnsupportedNegotiatedSchema { found: u16 },
    #[error("OWVALUE reserved field is nonzero: {found}")]
    NonzeroReserved { found: u16 },
    #[error("OWVALUE record length says {declared} bytes but input has {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("unknown OWVALUE node tag {found:#06x}")]
    UnknownTag { found: u16 },
    #[error("OWVALUE node {tag:#06x} reserved field is nonzero: {found}")]
    NonzeroNodeReserved { tag: u16, found: u16 },
    #[error(
        "OWVALUE node {tag:#06x} length {declared} is invalid for {remaining} remaining bytes"
    )]
    InvalidNodeLength {
        tag: u16,
        declared: usize,
        remaining: usize,
    },
    #[error("OWVALUE node {tag:#06x} payload is {actual} bytes; expected {expected}")]
    InvalidPayloadLength {
        tag: u16,
        expected: usize,
        actual: usize,
    },
    #[error("OWVALUE node nesting depth {actual} exceeds {maximum}")]
    DepthLimit { actual: usize, maximum: usize },
    #[error("OWVALUE node count {actual} exceeds {maximum}")]
    NodeLimit { actual: usize, maximum: usize },
    #[error("OWVALUE {kind} has {actual} entries; maximum is {maximum}")]
    EntryLimit {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("integer sign/magnitude is noncanonical")]
    InvalidInteger,
    #[error("integer magnitude is {actual} bytes; maximum is {maximum}")]
    IntegerTooLarge { actual: usize, maximum: usize },
    #[error("binary float format {format} has {actual} raw bytes")]
    InvalidBinaryFloat { format: u8, actual: usize },
    #[error("text encoding must be absent or exactly utf-8")]
    InvalidTextEncoding,
    #[error("text is {actual} UTF-8 bytes; maximum is {maximum}")]
    TextTooLong { actual: usize, maximum: usize },
    #[error("OWVALUE text is not valid UTF-8")]
    InvalidUtf8,
    #[error("{found:#x} is not a Unicode scalar value")]
    InvalidUnicodeScalar { found: u32 },
    #[error("byte media type is not canonical printable ASCII type/subtype")]
    InvalidMediaType,
    #[error("byte payload is {actual} bytes; maximum is {maximum}")]
    BytesTooLong { actual: usize, maximum: usize },
    #[error("{field} identifier is invalid")]
    InvalidIdentifier { field: &'static str },
    #[error("record key is empty, invalid UTF-8, or exceeds {maximum} bytes")]
    InvalidRecordKey { maximum: usize },
    #[error("record keys are not in strict canonical byte order")]
    NonCanonicalRecordOrder,
    #[error("record contains a duplicate key")]
    DuplicateRecordKey,
    #[error("map key is not one of the scalar PR4 key kinds")]
    NonScalarMapKey,
    #[error("map keys are not in strict canonical node-byte order")]
    NonCanonicalMapOrder,
    #[error("map contains a duplicate canonical key")]
    DuplicateMapKey,
    #[error("extension namespace must be canonical and contain a dot")]
    InvalidExtensionNamespace,
    #[error("extension version zero is reserved")]
    ZeroExtensionVersion,
    #[error("extension schema digest cannot be all zero")]
    ZeroExtensionSchemaDigest,
    #[error("decoded extension does not match the expected exact schema")]
    ExtensionSchemaMismatch,
    #[error("expected a root extension envelope, found a portable core value")]
    ExpectedExtension,
    #[error("extension envelopes are permitted only as the OWVALUE root")]
    NestedExtension,
    #[error("OWVALUE record has {remaining} trailing bytes")]
    TrailingBytes { remaining: usize },
    #[error("nested OWIDENT record is not an Object identity")]
    ExpectedObjectIdentity,
    #[error("nested OWIDENT Object record is noncanonical")]
    NonCanonicalObjectIdentity,
    #[error(transparent)]
    Identity(#[from] IdentityWireError),
}

impl TryFrom<&OValue> for PortableOValue {
    type Error = HostedValueError;

    fn try_from(value: &OValue) -> Result<Self, HostedValueError> {
        match value {
            OValue::Null => Ok(Self::Null),
            OValue::Bool { v } => Ok(Self::Bool(*v)),
            OValue::Number {
                v: number @ (ONumber::Int { .. } | ONumber::BinaryFloat { .. }),
            } => {
                let portable = Self::Number(number.clone());
                portable.validate()?;
                Ok(portable)
            }
            OValue::Number {
                v: ONumber::Rational { .. },
            } => Err(HostedValueError::Unsupported {
                kind: "number:rational",
            }),
            OValue::Number {
                v: ONumber::Decimal { .. },
            } => Err(HostedValueError::Unsupported {
                kind: "number:decimal",
            }),
            OValue::Number {
                v: ONumber::BigFloat { .. },
            } => Err(HostedValueError::Unsupported {
                kind: "number:big_float",
            }),
            OValue::Number {
                v: ONumber::Complex { .. },
            } => Err(HostedValueError::Unsupported {
                kind: "number:complex",
            }),
            OValue::Text { v } => {
                let portable = Self::Text(v.clone());
                portable.validate()?;
                Ok(portable)
            }
            OValue::Char { scalar } => Ok(Self::Char(*scalar)),
            OValue::Bytes { v } => {
                let portable = Self::Bytes(v.clone());
                portable.validate()?;
                Ok(portable)
            }
            OValue::List { v } => {
                let portable = Self::List(v.iter().map(Self::try_from).collect::<Result<_, _>>()?);
                portable.validate()?;
                Ok(portable)
            }
            OValue::Object { fields } => Self::record(
                fields
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), Self::try_from(value)?)))
                    .collect::<Result<_, HostedValueError>>()?,
            )
            .map_err(Into::into),
            OValue::Map { v } => Self::record(
                v.iter()
                    .map(|(key, value)| Ok((key.clone(), Self::try_from(value)?)))
                    .collect::<Result<_, HostedValueError>>()?,
            )
            .map_err(Into::into),
            OValue::EntriesMap { entries } => Self::map(
                entries
                    .iter()
                    .map(|(key, value)| Ok((Self::try_from(key)?, Self::try_from(value)?)))
                    .collect::<Result<_, HostedValueError>>()?,
            )
            .map_err(Into::into),

            OValue::Capability { .. } => {
                Err(HostedValueError::AuthorityBearing { kind: "capability" })
            }
            OValue::Native { .. } => Err(HostedValueError::CapsuleBound { kind: "native" }),
            OValue::System { .. } => Err(HostedValueError::CapsuleBound { kind: "system" }),
            OValue::StorePath { .. } => Err(HostedValueError::CapsuleBound { kind: "store_path" }),
            OValue::Derivation { .. } => Err(HostedValueError::CapsuleBound { kind: "derivation" }),
            OValue::Scope { .. } => Err(HostedValueError::Effectful { kind: "scope" }),
            OValue::Request { .. } => Err(HostedValueError::Effectful { kind: "request" }),
            OValue::Thunk { .. } => Err(HostedValueError::Effectful { kind: "thunk" }),
            OValue::Group { .. } => Err(HostedValueError::Effectful { kind: "group" }),
            OValue::Expr { .. } => Err(HostedValueError::Effectful { kind: "expr" }),
            OValue::NixExpr { .. } => Err(HostedValueError::Effectful { kind: "nix_expr" }),
            OValue::Error { .. } => Err(HostedValueError::Effectful { kind: "error" }),

            OValue::Html { .. } => Err(HostedValueError::Unsupported { kind: "html" }),
            OValue::Blob { .. } => Err(HostedValueError::Unsupported { kind: "blob" }),
            OValue::Seq { .. } => Err(HostedValueError::Unsupported { kind: "seq" }),
            OValue::Set { .. } => Err(HostedValueError::Unsupported { kind: "set" }),
            OValue::Symbol { .. } => Err(HostedValueError::Unsupported { kind: "symbol" }),
            OValue::Keyword { .. } => Err(HostedValueError::Unsupported { kind: "keyword" }),
            OValue::Snapshot { .. } => Err(HostedValueError::Unsupported { kind: "snapshot" }),
            OValue::Graph { .. } => Err(HostedValueError::Unsupported { kind: "graph" }),
        }
    }
}

pub(crate) fn number_integer(value: &ONumber) -> Option<&BigInt> {
    match value {
        ONumber::Int { v } => Some(v),
        _ => None,
    }
}

pub(crate) fn number_binary_float(value: &ONumber) -> Option<(FloatFormat, &[u8])> {
    match value {
        ONumber::BinaryFloat { format, bits } => Some((*format, bits)),
        _ => None,
    }
}
