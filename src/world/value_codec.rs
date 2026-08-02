//! Canonical, bounded wire encoding for authority-free World values.
//!
//! `OWVALUE` is intentionally separate from the frozen four-kind `OWPROTO`
//! v1 control codec. A negotiated World schema can admit this record family,
//! but the value bytes are independently framed and hashed. Decoding never
//! dispatches an extension or resolves descriptive data into authority.

use std::cmp::Ordering;

use num_bigint::{BigInt, Sign};
use sha2::{Digest, Sha256};

use crate::value::{FloatFormat, OBytes, ONumber, OText};

use super::identity_wire::{identity_v1_conformance_records, IdentityWireRecord};
use super::protocol::{NegotiatedSchema, WORLD_SCHEMA_V1};
use super::value::{
    number_binary_float, number_integer, AdmittedExtension, ExtensionEnvelope, PortableCodeRef,
    PortableError, PortableOValue, PortableTagged, PortableValueError, PortableValueRecord,
};

pub const OVALUE_WIRE_MAGIC: &[u8; 8] = b"OWVALUE\0";
pub const OVALUE_WIRE_SCHEMA_V1: u16 = 1;
pub const OVALUE_WIRE_HEADER_BYTES: usize = 16;
pub const OVALUE_NODE_HEADER_BYTES: usize = 8;
pub const MIN_OVALUE_RECORD_BYTES: u32 = 24;
pub const MAX_OVALUE_RECORD_BYTES: u32 = 4096;
pub const MAX_OVALUE_DEPTH: usize = 16;
pub const MAX_OVALUE_NODES: usize = 128;
pub const MAX_OVALUE_LIST_ITEMS: usize = 64;
pub const MAX_OVALUE_MAP_ENTRIES: usize = 32;
pub const MAX_OVALUE_INTEGER_BYTES: usize = 256;
pub const MAX_OVALUE_TEXT_BYTES: usize = 1024;
pub const MAX_OVALUE_BYTES_BYTES: usize = 2048;
pub const MAX_OVALUE_IDENTIFIER_BYTES: usize = 96;

const TAG_NULL: u16 = 0x0000;
const TAG_BOOL: u16 = 0x0001;
const TAG_INTEGER: u16 = 0x0010;
const TAG_BINARY_FLOAT: u16 = 0x0011;
const TAG_TEXT: u16 = 0x0020;
const TAG_CHAR: u16 = 0x0021;
const TAG_BYTES: u16 = 0x0022;
const TAG_LIST: u16 = 0x0030;
const TAG_RECORD: u16 = 0x0031;
const TAG_MAP: u16 = 0x0032;
const TAG_TAGGED: u16 = 0x0040;
const TAG_CODE_REF: u16 = 0x0041;
const TAG_OBJECT_REF: u16 = 0x0042;
const TAG_ERROR: u16 = 0x0043;
const TAG_EXTENSION: u16 = 0x7f00;

const FLOAT_F32: u8 = 1;
const FLOAT_F64: u8 = 2;
const CODE_DIGEST_SHA256: u16 = 1;

impl PortableOValue {
    pub fn integer(value: impl Into<BigInt>) -> Result<Self, PortableValueError> {
        let value = Self::Number(ONumber::Int { v: value.into() });
        value.validate()?;
        Ok(value)
    }

    pub fn binary_float(
        format: FloatFormat,
        bits: impl Into<Vec<u8>>,
    ) -> Result<Self, PortableValueError> {
        let value = Self::Number(ONumber::BinaryFloat {
            format,
            bits: bits.into(),
        });
        value.validate()?;
        Ok(value)
    }

    pub fn text(value: OText) -> Result<Self, PortableValueError> {
        let value = Self::Text(value);
        value.validate()?;
        Ok(value)
    }

    pub fn bytes(value: OBytes) -> Result<Self, PortableValueError> {
        let value = Self::Bytes(value);
        value.validate()?;
        Ok(value)
    }

    pub fn record(mut fields: Vec<(String, PortableOValue)>) -> Result<Self, PortableValueError> {
        if fields.len() > MAX_OVALUE_MAP_ENTRIES {
            return Err(PortableValueError::EntryLimit {
                kind: "record",
                actual: fields.len(),
                maximum: MAX_OVALUE_MAP_ENTRIES,
            });
        }
        for (key, value) in &fields {
            validate_record_key(key.as_bytes())?;
            value.validate()?;
        }
        fields.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
        for pair in fields.windows(2) {
            if pair[0].0.as_bytes() == pair[1].0.as_bytes() {
                return Err(PortableValueError::DuplicateRecordKey);
            }
        }
        let value = Self::Record(fields);
        value.validate()?;
        Ok(value)
    }

    pub fn map(
        mut entries: Vec<(PortableOValue, PortableOValue)>,
    ) -> Result<Self, PortableValueError> {
        if entries.len() > MAX_OVALUE_MAP_ENTRIES {
            return Err(PortableValueError::EntryLimit {
                kind: "map",
                actual: entries.len(),
                maximum: MAX_OVALUE_MAP_ENTRIES,
            });
        }
        let mut keyed = Vec::with_capacity(entries.len());
        for (key, value) in entries.drain(..) {
            if !is_scalar_map_key(&key) {
                return Err(PortableValueError::NonScalarMapKey);
            }
            key.validate()?;
            value.validate()?;
            keyed.push((encode_one_node(&key)?, key, value));
        }
        keyed.sort_by(|left, right| left.0.cmp(&right.0));
        for pair in keyed.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(PortableValueError::DuplicateMapKey);
            }
        }
        let value = Self::Map(
            keyed
                .into_iter()
                .map(|(_, key, value)| (key, value))
                .collect(),
        );
        value.validate()?;
        Ok(value)
    }

    pub fn tagged(
        tag: impl Into<String>,
        value: PortableOValue,
    ) -> Result<Self, PortableValueError> {
        let value = Self::Tagged(PortableTagged {
            tag: tag.into(),
            value: Box::new(value),
        });
        value.validate()?;
        Ok(value)
    }

    pub fn code_ref(
        digest: [u8; 32],
        evaluator: impl Into<String>,
        entrypoint: impl Into<String>,
    ) -> Result<Self, PortableValueError> {
        let value = Self::CodeRef(PortableCodeRef {
            digest,
            evaluator: evaluator.into(),
            entrypoint: entrypoint.into(),
        });
        value.validate()?;
        Ok(value)
    }

    pub fn error(
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, PortableValueError> {
        let value = Self::Error(PortableError {
            code: code.into(),
            message: message.into(),
        });
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), PortableValueError> {
        let encoded = encode_one_node(self)?;
        let total = OVALUE_WIRE_HEADER_BYTES + encoded.len();
        if total > MAX_OVALUE_RECORD_BYTES as usize {
            return Err(PortableValueError::RecordTooLarge {
                actual: total,
                maximum: MAX_OVALUE_RECORD_BYTES as usize,
            });
        }
        Ok(())
    }

    pub fn canonical_node_bytes(&self) -> Result<Vec<u8>, PortableValueError> {
        encode_one_node(self)
    }
}

impl ExtensionEnvelope {
    pub fn new(
        namespace: impl Into<String>,
        name: impl Into<String>,
        version: u16,
        schema_digest: [u8; 32],
        value: PortableOValue,
    ) -> Result<Self, PortableValueError> {
        let envelope = Self {
            namespace: namespace.into(),
            name: name.into(),
            version,
            schema_digest,
            value: Box::new(value),
        };
        PortableValueRecord::Extension(envelope.clone()).validate()?;
        Ok(envelope)
    }
}

impl PortableValueRecord {
    pub fn validate(&self) -> Result<(), PortableValueError> {
        self.encode().map(|_| ())
    }

    pub fn encode(&self) -> Result<Vec<u8>, PortableValueError> {
        self.encode_with_limit(MAX_OVALUE_RECORD_BYTES)
    }

    pub fn encode_with_limit(&self, limit: u32) -> Result<Vec<u8>, PortableValueError> {
        let mut body = Vec::new();
        let mut state = EncodeState { nodes: 0 };
        match self {
            Self::Core(value) => encode_core_node(value, &mut body, 1, &mut state)?,
            Self::Extension(envelope) => encode_extension_node(envelope, &mut body, 1, &mut state)?,
        }
        let total = OVALUE_WIRE_HEADER_BYTES.checked_add(body.len()).ok_or(
            PortableValueError::RecordTooLarge {
                actual: usize::MAX,
                maximum: MAX_OVALUE_RECORD_BYTES as usize,
            },
        )?;
        enforce_record_size(total, limit)?;
        let total_u32 = u32::try_from(total).map_err(|_| PortableValueError::RecordTooLarge {
            actual: total,
            maximum: MAX_OVALUE_RECORD_BYTES as usize,
        })?;
        let mut output = Vec::with_capacity(total);
        output.extend_from_slice(OVALUE_WIRE_MAGIC);
        put_u16(&mut output, OVALUE_WIRE_SCHEMA_V1);
        put_u16(&mut output, 0);
        put_u32(&mut output, total_u32);
        output.extend_from_slice(&body);
        Ok(output)
    }

    pub fn decode(record: &[u8]) -> Result<Self, PortableValueError> {
        Self::decode_with_limit(record, MAX_OVALUE_RECORD_BYTES)
    }

    pub fn decode_with_limit(record: &[u8], limit: u32) -> Result<Self, PortableValueError> {
        enforce_record_size(record.len(), limit)?;
        if record.len() < OVALUE_WIRE_HEADER_BYTES {
            return Err(PortableValueError::Truncated {
                needed: OVALUE_WIRE_HEADER_BYTES,
                remaining: record.len(),
            });
        }
        if &record[..8] != OVALUE_WIRE_MAGIC {
            return Err(PortableValueError::BadMagic);
        }
        let schema = read_u16_at(record, 8);
        if schema != OVALUE_WIRE_SCHEMA_V1 {
            return Err(PortableValueError::UnsupportedSchema { found: schema });
        }
        let reserved = read_u16_at(record, 10);
        if reserved != 0 {
            return Err(PortableValueError::NonzeroReserved { found: reserved });
        }
        let declared = read_u32_at(record, 12) as usize;
        if declared != record.len() {
            return Err(PortableValueError::LengthMismatch {
                declared,
                actual: record.len(),
            });
        }
        let mut cursor = Cursor::new(&record[OVALUE_WIRE_HEADER_BYTES..]);
        let mut state = DecodeState { nodes: 0 };
        let decoded = decode_root_node(&mut cursor, 1, &mut state)?;
        if cursor.remaining() != 0 {
            return Err(PortableValueError::TrailingBytes {
                remaining: cursor.remaining(),
            });
        }
        // Strict parsing should already imply this equality. Re-encoding is a
        // second canonicality boundary and catches future parser drift.
        let canonical = decoded.encode()?;
        if canonical != record {
            return Err(PortableValueError::LengthMismatch {
                declared: canonical.len(),
                actual: record.len(),
            });
        }
        Ok(decoded)
    }

    pub fn decode_with_negotiated_schema(
        record: &[u8],
        negotiated: NegotiatedSchema,
    ) -> Result<Self, PortableValueError> {
        if negotiated.version() != WORLD_SCHEMA_V1 {
            return Err(PortableValueError::UnsupportedNegotiatedSchema {
                found: negotiated.version(),
            });
        }
        Self::decode_with_limit(
            record,
            negotiated.max_record_bytes().min(MAX_OVALUE_RECORD_BYTES),
        )
    }

    pub fn decode_extension_with_negotiated_schema(
        record: &[u8],
        negotiated: NegotiatedSchema,
        namespace: &str,
        name: &str,
        version: u16,
        schema_digest: &[u8; 32],
    ) -> Result<AdmittedExtension, PortableValueError> {
        match Self::decode_with_negotiated_schema(record, negotiated)? {
            Self::Extension(envelope) => {
                envelope.admit_exact(namespace, name, version, schema_digest)
            }
            Self::Core(_) => Err(PortableValueError::ExpectedExtension),
        }
    }

    pub fn canonical_sha256(&self) -> Result<[u8; 32], PortableValueError> {
        let digest = Sha256::digest(self.encode()?);
        Ok(digest.into())
    }
}

struct EncodeState {
    nodes: usize,
}

fn encode_one_node(value: &PortableOValue) -> Result<Vec<u8>, PortableValueError> {
    let mut output = Vec::new();
    let mut state = EncodeState { nodes: 0 };
    encode_core_node(value, &mut output, 1, &mut state)?;
    Ok(output)
}

fn begin_node(output: &mut Vec<u8>, tag: u16) -> usize {
    let start = output.len();
    put_u16(output, tag);
    put_u16(output, 0);
    put_u32(output, 0);
    start
}

fn finish_node(output: &mut [u8], start: usize) -> Result<(), PortableValueError> {
    let total = output.len().saturating_sub(start);
    let total_u32 = u32::try_from(total).map_err(|_| PortableValueError::RecordTooLarge {
        actual: total,
        maximum: MAX_OVALUE_RECORD_BYTES as usize,
    })?;
    output[start + 4..start + 8].copy_from_slice(&total_u32.to_be_bytes());
    Ok(())
}

fn enter_encode_node(depth: usize, state: &mut EncodeState) -> Result<(), PortableValueError> {
    if depth > MAX_OVALUE_DEPTH {
        return Err(PortableValueError::DepthLimit {
            actual: depth,
            maximum: MAX_OVALUE_DEPTH,
        });
    }
    state.nodes += 1;
    if state.nodes > MAX_OVALUE_NODES {
        return Err(PortableValueError::NodeLimit {
            actual: state.nodes,
            maximum: MAX_OVALUE_NODES,
        });
    }
    Ok(())
}

fn encode_core_node(
    value: &PortableOValue,
    output: &mut Vec<u8>,
    depth: usize,
    state: &mut EncodeState,
) -> Result<(), PortableValueError> {
    enter_encode_node(depth, state)?;
    let tag = core_tag(value)?;
    let start = begin_node(output, tag);
    match value {
        PortableOValue::Null => {}
        PortableOValue::Bool(value) => output.push(u8::from(*value)),
        PortableOValue::Number(number) => {
            if let Some(integer) = number_integer(number) {
                let (sign, magnitude) = integer.to_bytes_be();
                if magnitude.len() > MAX_OVALUE_INTEGER_BYTES {
                    return Err(PortableValueError::IntegerTooLarge {
                        actual: magnitude.len(),
                        maximum: MAX_OVALUE_INTEGER_BYTES,
                    });
                }
                match sign {
                    Sign::NoSign => output.push(0),
                    Sign::Plus => {
                        output.push(0);
                        output.extend_from_slice(&magnitude);
                    }
                    Sign::Minus => {
                        output.push(1);
                        output.extend_from_slice(&magnitude);
                    }
                }
            } else if let Some((format, bits)) = number_binary_float(number) {
                let (format_tag, expected) = match format {
                    FloatFormat::F32 => (FLOAT_F32, 4),
                    FloatFormat::F64 => (FLOAT_F64, 8),
                };
                if bits.len() != expected {
                    return Err(PortableValueError::InvalidBinaryFloat {
                        format: format_tag,
                        actual: bits.len(),
                    });
                }
                output.push(format_tag);
                output.extend_from_slice(&[0, 0, 0]);
                output.extend_from_slice(bits);
            } else {
                return Err(PortableValueError::InvalidInteger);
            }
        }
        PortableOValue::Text(text) => {
            let encoding = match text.encoding.as_deref() {
                None => 0,
                Some("utf-8") => 1,
                Some(_) => return Err(PortableValueError::InvalidTextEncoding),
            };
            if text.utf8.len() > MAX_OVALUE_TEXT_BYTES {
                return Err(PortableValueError::TextTooLong {
                    actual: text.utf8.len(),
                    maximum: MAX_OVALUE_TEXT_BYTES,
                });
            }
            output.push(encoding);
            output.extend_from_slice(&[0, 0, 0]);
            output.extend_from_slice(text.utf8.as_bytes());
        }
        PortableOValue::Char(value) => put_u32(output, *value as u32),
        PortableOValue::Bytes(bytes) => {
            let media = bytes.media_type.as_deref().unwrap_or("");
            validate_media_type(bytes.media_type.as_deref())?;
            if bytes.bytes.len() > MAX_OVALUE_BYTES_BYTES {
                return Err(PortableValueError::BytesTooLong {
                    actual: bytes.bytes.len(),
                    maximum: MAX_OVALUE_BYTES_BYTES,
                });
            }
            put_u16(
                output,
                u16::try_from(media.len()).map_err(|_| PortableValueError::InvalidMediaType)?,
            );
            put_u16(output, 0);
            output.extend_from_slice(media.as_bytes());
            output.extend_from_slice(&bytes.bytes);
        }
        PortableOValue::List(items) => {
            validate_entry_count("list", items.len(), MAX_OVALUE_LIST_ITEMS)?;
            put_u16(output, items.len() as u16);
            put_u16(output, 0);
            for child in items {
                encode_core_node(child, output, depth + 1, state)?;
            }
        }
        PortableOValue::Record(fields) => {
            validate_entry_count("record", fields.len(), MAX_OVALUE_MAP_ENTRIES)?;
            put_u16(output, fields.len() as u16);
            put_u16(output, 0);
            let mut prior: Option<&[u8]> = None;
            for (key, child) in fields {
                let key_bytes = key.as_bytes();
                validate_record_key(key_bytes)?;
                if let Some(previous) = prior {
                    match previous.cmp(key_bytes) {
                        Ordering::Less => {}
                        Ordering::Equal => return Err(PortableValueError::DuplicateRecordKey),
                        Ordering::Greater => {
                            return Err(PortableValueError::NonCanonicalRecordOrder)
                        }
                    }
                }
                prior = Some(key_bytes);
                put_u16(output, key_bytes.len() as u16);
                output.extend_from_slice(key_bytes);
                encode_core_node(child, output, depth + 1, state)?;
            }
        }
        PortableOValue::Map(entries) => {
            validate_entry_count("map", entries.len(), MAX_OVALUE_MAP_ENTRIES)?;
            put_u16(output, entries.len() as u16);
            put_u16(output, 0);
            let mut prior: Option<Vec<u8>> = None;
            for (key, value) in entries {
                if !is_scalar_map_key(key) {
                    return Err(PortableValueError::NonScalarMapKey);
                }
                let key_start = output.len();
                encode_core_node(key, output, depth + 1, state)?;
                let key_bytes = output[key_start..].to_vec();
                if let Some(previous) = &prior {
                    match previous.cmp(&key_bytes) {
                        Ordering::Less => {}
                        Ordering::Equal => return Err(PortableValueError::DuplicateMapKey),
                        Ordering::Greater => return Err(PortableValueError::NonCanonicalMapOrder),
                    }
                }
                prior = Some(key_bytes);
                encode_core_node(value, output, depth + 1, state)?;
            }
        }
        PortableOValue::Tagged(tagged) => {
            validate_identifier(&tagged.tag, "tag")?;
            put_u16(output, tagged.tag.len() as u16);
            put_u16(output, 0);
            output.extend_from_slice(tagged.tag.as_bytes());
            encode_core_node(&tagged.value, output, depth + 1, state)?;
        }
        PortableOValue::CodeRef(reference) => {
            validate_identifier(&reference.evaluator, "code evaluator")?;
            validate_identifier(&reference.entrypoint, "code entrypoint")?;
            put_u16(output, CODE_DIGEST_SHA256);
            put_u16(output, 0);
            put_u16(output, reference.evaluator.len() as u16);
            put_u16(output, reference.entrypoint.len() as u16);
            output.extend_from_slice(&reference.digest);
            output.extend_from_slice(reference.evaluator.as_bytes());
            output.extend_from_slice(reference.entrypoint.as_bytes());
        }
        PortableOValue::ObjectRef(identity) => {
            output.extend_from_slice(&IdentityWireRecord::Object(identity.clone()).encode()?);
        }
        PortableOValue::Error(error) => {
            validate_identifier(&error.code, "error code")?;
            if error.message.len() > MAX_OVALUE_TEXT_BYTES {
                return Err(PortableValueError::TextTooLong {
                    actual: error.message.len(),
                    maximum: MAX_OVALUE_TEXT_BYTES,
                });
            }
            put_u16(output, error.code.len() as u16);
            put_u16(output, 0);
            output.extend_from_slice(error.code.as_bytes());
            output.extend_from_slice(error.message.as_bytes());
        }
    }
    finish_node(output, start)
}

fn encode_extension_node(
    envelope: &ExtensionEnvelope,
    output: &mut Vec<u8>,
    depth: usize,
    state: &mut EncodeState,
) -> Result<(), PortableValueError> {
    enter_encode_node(depth, state)?;
    validate_extension(envelope)?;
    let start = begin_node(output, TAG_EXTENSION);
    put_u16(output, envelope.namespace.len() as u16);
    put_u16(output, envelope.name.len() as u16);
    put_u16(output, envelope.version);
    put_u16(output, 0);
    output.extend_from_slice(&envelope.schema_digest);
    output.extend_from_slice(envelope.namespace.as_bytes());
    output.extend_from_slice(envelope.name.as_bytes());
    encode_core_node(&envelope.value, output, depth + 1, state)?;
    finish_node(output, start)
}

fn core_tag(value: &PortableOValue) -> Result<u16, PortableValueError> {
    match value {
        PortableOValue::Null => Ok(TAG_NULL),
        PortableOValue::Bool(_) => Ok(TAG_BOOL),
        PortableOValue::Number(number) if number_integer(number).is_some() => Ok(TAG_INTEGER),
        PortableOValue::Number(number) if number_binary_float(number).is_some() => {
            Ok(TAG_BINARY_FLOAT)
        }
        PortableOValue::Number(_) => Err(PortableValueError::InvalidInteger),
        PortableOValue::Text(_) => Ok(TAG_TEXT),
        PortableOValue::Char(_) => Ok(TAG_CHAR),
        PortableOValue::Bytes(_) => Ok(TAG_BYTES),
        PortableOValue::List(_) => Ok(TAG_LIST),
        PortableOValue::Record(_) => Ok(TAG_RECORD),
        PortableOValue::Map(_) => Ok(TAG_MAP),
        PortableOValue::Tagged(_) => Ok(TAG_TAGGED),
        PortableOValue::CodeRef(_) => Ok(TAG_CODE_REF),
        PortableOValue::ObjectRef(_) => Ok(TAG_OBJECT_REF),
        PortableOValue::Error(_) => Ok(TAG_ERROR),
    }
}

struct DecodeState {
    nodes: usize,
}

enum DecodedRoot {
    Core(PortableOValue),
    Extension(ExtensionEnvelope),
}

impl From<DecodedRoot> for PortableValueRecord {
    fn from(value: DecodedRoot) -> Self {
        match value {
            DecodedRoot::Core(value) => Self::Core(value),
            DecodedRoot::Extension(value) => Self::Extension(value),
        }
    }
}

fn decode_root_node(
    cursor: &mut Cursor<'_>,
    depth: usize,
    state: &mut DecodeState,
) -> Result<PortableValueRecord, PortableValueError> {
    let (tag, mut payload) = take_node(cursor, depth, state)?;
    let decoded = if tag == TAG_EXTENSION {
        DecodedRoot::Extension(decode_extension_payload(&mut payload, depth, state)?)
    } else {
        DecodedRoot::Core(decode_core_payload(tag, &mut payload, depth, state)?)
    };
    if payload.remaining() != 0 {
        return Err(PortableValueError::TrailingBytes {
            remaining: payload.remaining(),
        });
    }
    Ok(decoded.into())
}

fn decode_core_node(
    cursor: &mut Cursor<'_>,
    depth: usize,
    state: &mut DecodeState,
) -> Result<PortableOValue, PortableValueError> {
    let (tag, mut payload) = take_node(cursor, depth, state)?;
    if tag == TAG_EXTENSION {
        return Err(PortableValueError::NestedExtension);
    }
    let decoded = decode_core_payload(tag, &mut payload, depth, state)?;
    if payload.remaining() != 0 {
        return Err(PortableValueError::TrailingBytes {
            remaining: payload.remaining(),
        });
    }
    Ok(decoded)
}

fn take_node<'a>(
    cursor: &mut Cursor<'a>,
    depth: usize,
    state: &mut DecodeState,
) -> Result<(u16, Cursor<'a>), PortableValueError> {
    if depth > MAX_OVALUE_DEPTH {
        return Err(PortableValueError::DepthLimit {
            actual: depth,
            maximum: MAX_OVALUE_DEPTH,
        });
    }
    state.nodes += 1;
    if state.nodes > MAX_OVALUE_NODES {
        return Err(PortableValueError::NodeLimit {
            actual: state.nodes,
            maximum: MAX_OVALUE_NODES,
        });
    }
    if cursor.remaining() < OVALUE_NODE_HEADER_BYTES {
        return Err(PortableValueError::Truncated {
            needed: OVALUE_NODE_HEADER_BYTES,
            remaining: cursor.remaining(),
        });
    }
    let tag = cursor.take_u16()?;
    let reserved = cursor.take_u16()?;
    if reserved != 0 {
        return Err(PortableValueError::NonzeroNodeReserved {
            tag,
            found: reserved,
        });
    }
    let declared = cursor.take_u32()? as usize;
    if declared < OVALUE_NODE_HEADER_BYTES
        || declared - OVALUE_NODE_HEADER_BYTES > cursor.remaining()
    {
        return Err(PortableValueError::InvalidNodeLength {
            tag,
            declared,
            remaining: cursor.remaining() + OVALUE_NODE_HEADER_BYTES,
        });
    }
    Ok((
        tag,
        Cursor::new(cursor.take(declared - OVALUE_NODE_HEADER_BYTES)?),
    ))
}

fn decode_core_payload(
    tag: u16,
    payload: &mut Cursor<'_>,
    depth: usize,
    state: &mut DecodeState,
) -> Result<PortableOValue, PortableValueError> {
    match tag {
        TAG_NULL => {
            require_payload(tag, payload, 0)?;
            Ok(PortableOValue::Null)
        }
        TAG_BOOL => {
            require_payload(tag, payload, 1)?;
            match payload.take_u8()? {
                0 => Ok(PortableOValue::Bool(false)),
                1 => Ok(PortableOValue::Bool(true)),
                _ => Err(PortableValueError::InvalidPayloadLength {
                    tag,
                    expected: 1,
                    actual: 1,
                }),
            }
        }
        TAG_INTEGER => {
            if payload.remaining() < 1 {
                return Err(PortableValueError::InvalidInteger);
            }
            let sign = payload.take_u8()?;
            let magnitude = payload.take_remaining();
            if magnitude.len() > MAX_OVALUE_INTEGER_BYTES {
                return Err(PortableValueError::IntegerTooLarge {
                    actual: magnitude.len(),
                    maximum: MAX_OVALUE_INTEGER_BYTES,
                });
            }
            if sign > 1
                || (!magnitude.is_empty() && magnitude[0] == 0)
                || (magnitude.is_empty() && sign != 0)
            {
                return Err(PortableValueError::InvalidInteger);
            }
            let value = if magnitude.is_empty() {
                BigInt::from(0)
            } else {
                BigInt::from_bytes_be(if sign == 1 { Sign::Minus } else { Sign::Plus }, magnitude)
            };
            Ok(PortableOValue::Number(ONumber::Int { v: value }))
        }
        TAG_BINARY_FLOAT => {
            if payload.remaining() < 4 {
                return Err(PortableValueError::InvalidBinaryFloat {
                    format: 0,
                    actual: payload.remaining(),
                });
            }
            let format_tag = payload.take_u8()?;
            if payload.take_u8()? != 0 || payload.take_u16()? != 0 {
                return Err(PortableValueError::NonzeroNodeReserved { tag, found: 1 });
            }
            let bits = payload.take_remaining().to_vec();
            let format = match (format_tag, bits.len()) {
                (FLOAT_F32, 4) => FloatFormat::F32,
                (FLOAT_F64, 8) => FloatFormat::F64,
                _ => {
                    return Err(PortableValueError::InvalidBinaryFloat {
                        format: format_tag,
                        actual: bits.len(),
                    })
                }
            };
            Ok(PortableOValue::Number(ONumber::BinaryFloat {
                format,
                bits,
            }))
        }
        TAG_TEXT => {
            if payload.remaining() < 4 {
                return Err(PortableValueError::Truncated {
                    needed: 4,
                    remaining: payload.remaining(),
                });
            }
            let encoding = payload.take_u8()?;
            if payload.take_u8()? != 0 || payload.take_u16()? != 0 {
                return Err(PortableValueError::NonzeroNodeReserved { tag, found: 1 });
            }
            let text = take_utf8_remaining(payload)?;
            if text.len() > MAX_OVALUE_TEXT_BYTES {
                return Err(PortableValueError::TextTooLong {
                    actual: text.len(),
                    maximum: MAX_OVALUE_TEXT_BYTES,
                });
            }
            let encoding = match encoding {
                0 => None,
                1 => Some("utf-8".to_owned()),
                _ => return Err(PortableValueError::InvalidTextEncoding),
            };
            Ok(PortableOValue::Text(OText {
                utf8: text.to_owned(),
                encoding,
            }))
        }
        TAG_CHAR => {
            require_payload(tag, payload, 4)?;
            let scalar = payload.take_u32()?;
            let value = char::from_u32(scalar)
                .ok_or(PortableValueError::InvalidUnicodeScalar { found: scalar })?;
            Ok(PortableOValue::Char(value))
        }
        TAG_BYTES => {
            if payload.remaining() < 4 {
                return Err(PortableValueError::Truncated {
                    needed: 4,
                    remaining: payload.remaining(),
                });
            }
            let media_len = payload.take_u16()? as usize;
            let reserved = payload.take_u16()?;
            if reserved != 0 {
                return Err(PortableValueError::NonzeroNodeReserved {
                    tag,
                    found: reserved,
                });
            }
            let media = if media_len == 0 {
                None
            } else {
                Some(
                    std::str::from_utf8(payload.take(media_len)?)
                        .map_err(|_| PortableValueError::InvalidMediaType)?
                        .to_owned(),
                )
            };
            validate_media_type(media.as_deref())?;
            let bytes = payload.take_remaining().to_vec();
            if bytes.len() > MAX_OVALUE_BYTES_BYTES {
                return Err(PortableValueError::BytesTooLong {
                    actual: bytes.len(),
                    maximum: MAX_OVALUE_BYTES_BYTES,
                });
            }
            Ok(PortableOValue::Bytes(OBytes {
                bytes,
                media_type: media,
            }))
        }
        TAG_LIST => {
            let count = take_collection_prefix(tag, payload)?;
            validate_entry_count("list", count, MAX_OVALUE_LIST_ITEMS)?;
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push(decode_core_node(payload, depth + 1, state)?);
            }
            Ok(PortableOValue::List(values))
        }
        TAG_RECORD => {
            let count = take_collection_prefix(tag, payload)?;
            validate_entry_count("record", count, MAX_OVALUE_MAP_ENTRIES)?;
            let mut fields = Vec::with_capacity(count);
            let mut prior: Option<Vec<u8>> = None;
            for _ in 0..count {
                let key_len = payload.take_u16()? as usize;
                let key_bytes = payload.take(key_len)?;
                validate_record_key(key_bytes)?;
                if let Some(previous) = &prior {
                    match previous.as_slice().cmp(key_bytes) {
                        Ordering::Less => {}
                        Ordering::Equal => return Err(PortableValueError::DuplicateRecordKey),
                        Ordering::Greater => {
                            return Err(PortableValueError::NonCanonicalRecordOrder)
                        }
                    }
                }
                prior = Some(key_bytes.to_vec());
                let key = std::str::from_utf8(key_bytes)
                    .map_err(|_| PortableValueError::InvalidRecordKey {
                        maximum: MAX_OVALUE_IDENTIFIER_BYTES,
                    })?
                    .to_owned();
                fields.push((key, decode_core_node(payload, depth + 1, state)?));
            }
            Ok(PortableOValue::Record(fields))
        }
        TAG_MAP => {
            let count = take_collection_prefix(tag, payload)?;
            validate_entry_count("map", count, MAX_OVALUE_MAP_ENTRIES)?;
            let mut entries = Vec::with_capacity(count);
            let mut prior: Option<Vec<u8>> = None;
            for _ in 0..count {
                let key_start = payload.offset;
                let key = decode_core_node(payload, depth + 1, state)?;
                if !is_scalar_map_key(&key) {
                    return Err(PortableValueError::NonScalarMapKey);
                }
                let key_bytes = payload.bytes[key_start..payload.offset].to_vec();
                if let Some(previous) = &prior {
                    match previous.cmp(&key_bytes) {
                        Ordering::Less => {}
                        Ordering::Equal => return Err(PortableValueError::DuplicateMapKey),
                        Ordering::Greater => return Err(PortableValueError::NonCanonicalMapOrder),
                    }
                }
                prior = Some(key_bytes);
                let value = decode_core_node(payload, depth + 1, state)?;
                entries.push((key, value));
            }
            Ok(PortableOValue::Map(entries))
        }
        TAG_TAGGED => {
            let (tag_text, _) = take_identifier(payload, "tag")?;
            let value = decode_core_node(payload, depth + 1, state)?;
            Ok(PortableOValue::Tagged(PortableTagged {
                tag: tag_text,
                value: Box::new(value),
            }))
        }
        TAG_CODE_REF => {
            if payload.remaining() < 40 {
                return Err(PortableValueError::Truncated {
                    needed: 40,
                    remaining: payload.remaining(),
                });
            }
            let algorithm = payload.take_u16()?;
            let reserved = payload.take_u16()?;
            let evaluator_len = payload.take_u16()? as usize;
            let entrypoint_len = payload.take_u16()? as usize;
            if algorithm != CODE_DIGEST_SHA256 || reserved != 0 {
                return Err(PortableValueError::NonzeroNodeReserved {
                    tag,
                    found: reserved.max(algorithm.saturating_sub(CODE_DIGEST_SHA256)),
                });
            }
            let mut digest = [0_u8; 32];
            digest.copy_from_slice(payload.take(32)?);
            let evaluator = take_identifier_exact(payload, evaluator_len, "code evaluator")?;
            let entrypoint = take_identifier_exact(payload, entrypoint_len, "code entrypoint")?;
            Ok(PortableOValue::CodeRef(PortableCodeRef {
                digest,
                evaluator,
                entrypoint,
            }))
        }
        TAG_OBJECT_REF => {
            let nested = payload.take_remaining();
            let decoded = IdentityWireRecord::decode(nested)?;
            let IdentityWireRecord::Object(identity) = decoded else {
                return Err(PortableValueError::ExpectedObjectIdentity);
            };
            if IdentityWireRecord::Object(identity.clone())
                .encode()?
                .as_slice()
                != nested
            {
                return Err(PortableValueError::NonCanonicalObjectIdentity);
            }
            Ok(PortableOValue::ObjectRef(identity))
        }
        TAG_ERROR => {
            if payload.remaining() < 4 {
                return Err(PortableValueError::Truncated {
                    needed: 4,
                    remaining: payload.remaining(),
                });
            }
            let code_len = payload.take_u16()? as usize;
            let reserved = payload.take_u16()?;
            if reserved != 0 {
                return Err(PortableValueError::NonzeroNodeReserved {
                    tag,
                    found: reserved,
                });
            }
            let code = take_identifier_exact(payload, code_len, "error code")?;
            let message = take_utf8_remaining(payload)?.to_owned();
            if message.len() > MAX_OVALUE_TEXT_BYTES {
                return Err(PortableValueError::TextTooLong {
                    actual: message.len(),
                    maximum: MAX_OVALUE_TEXT_BYTES,
                });
            }
            Ok(PortableOValue::Error(PortableError { code, message }))
        }
        TAG_EXTENSION => Err(PortableValueError::NestedExtension),
        found => Err(PortableValueError::UnknownTag { found }),
    }
}

fn decode_extension_payload(
    payload: &mut Cursor<'_>,
    depth: usize,
    state: &mut DecodeState,
) -> Result<ExtensionEnvelope, PortableValueError> {
    if payload.remaining() < 40 {
        return Err(PortableValueError::Truncated {
            needed: 40,
            remaining: payload.remaining(),
        });
    }
    let namespace_len = payload.take_u16()? as usize;
    let name_len = payload.take_u16()? as usize;
    let version = payload.take_u16()?;
    let reserved = payload.take_u16()?;
    if version == 0 {
        return Err(PortableValueError::ZeroExtensionVersion);
    }
    if reserved != 0 {
        return Err(PortableValueError::NonzeroNodeReserved {
            tag: TAG_EXTENSION,
            found: reserved,
        });
    }
    let mut schema_digest = [0_u8; 32];
    schema_digest.copy_from_slice(payload.take(32)?);
    if schema_digest.iter().all(|byte| *byte == 0) {
        return Err(PortableValueError::ZeroExtensionSchemaDigest);
    }
    let namespace = take_identifier_exact(payload, namespace_len, "extension namespace")?;
    if !namespace.contains('.') {
        return Err(PortableValueError::InvalidExtensionNamespace);
    }
    let name = take_identifier_exact(payload, name_len, "extension name")?;
    let value = decode_core_node(payload, depth + 1, state)?;
    Ok(ExtensionEnvelope {
        namespace,
        name,
        version,
        schema_digest,
        value: Box::new(value),
    })
}

fn validate_extension(envelope: &ExtensionEnvelope) -> Result<(), PortableValueError> {
    validate_identifier(&envelope.namespace, "extension namespace")?;
    if !envelope.namespace.contains('.') {
        return Err(PortableValueError::InvalidExtensionNamespace);
    }
    validate_identifier(&envelope.name, "extension name")?;
    if envelope.version == 0 {
        return Err(PortableValueError::ZeroExtensionVersion);
    }
    if envelope.schema_digest.iter().all(|byte| *byte == 0) {
        return Err(PortableValueError::ZeroExtensionSchemaDigest);
    }
    Ok(())
}

fn validate_entry_count(
    kind: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), PortableValueError> {
    if actual > maximum {
        return Err(PortableValueError::EntryLimit {
            kind,
            actual,
            maximum,
        });
    }
    Ok(())
}

fn validate_record_key(bytes: &[u8]) -> Result<(), PortableValueError> {
    if bytes.is_empty()
        || bytes.len() > MAX_OVALUE_IDENTIFIER_BYTES
        || std::str::from_utf8(bytes).is_err()
    {
        return Err(PortableValueError::InvalidRecordKey {
            maximum: MAX_OVALUE_IDENTIFIER_BYTES,
        });
    }
    Ok(())
}

fn validate_identifier(value: &str, field: &'static str) -> Result<(), PortableValueError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_OVALUE_IDENTIFIER_BYTES
        || !bytes[0].is_ascii_lowercase()
        || !bytes[1..].iter().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(*byte, b'.' | b'_' | b'-')
        })
    {
        return Err(PortableValueError::InvalidIdentifier { field });
    }
    Ok(())
}

fn validate_media_type(value: Option<&str>) -> Result<(), PortableValueError> {
    let Some(value) = value else {
        return Ok(());
    };
    let mut parts = value.split('/');
    let type_name = parts.next().unwrap_or_default();
    let subtype_name = parts.next().unwrap_or_default();
    if value.is_empty()
        || value.len() > MAX_OVALUE_IDENTIFIER_BYTES
        || type_name.is_empty()
        || subtype_name.is_empty()
        || parts.next().is_some()
        || !value
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte) && !byte.is_ascii_whitespace())
    {
        return Err(PortableValueError::InvalidMediaType);
    }
    Ok(())
}

fn is_scalar_map_key(value: &PortableOValue) -> bool {
    matches!(
        value,
        PortableOValue::Null
            | PortableOValue::Bool(_)
            | PortableOValue::Number(_)
            | PortableOValue::Text(_)
            | PortableOValue::Char(_)
            | PortableOValue::Bytes(_)
            | PortableOValue::CodeRef(_)
            | PortableOValue::ObjectRef(_)
    )
}

fn enforce_record_size(actual: usize, limit: u32) -> Result<(), PortableValueError> {
    if actual > MAX_OVALUE_RECORD_BYTES as usize {
        return Err(PortableValueError::RecordTooLarge {
            actual,
            maximum: MAX_OVALUE_RECORD_BYTES as usize,
        });
    }
    if actual < MIN_OVALUE_RECORD_BYTES as usize {
        return Err(PortableValueError::RecordTooSmall {
            actual,
            minimum: MIN_OVALUE_RECORD_BYTES as usize,
        });
    }
    if actual > limit as usize {
        return Err(PortableValueError::RecordExceedsLimit { actual, limit });
    }
    Ok(())
}

fn require_payload(
    tag: u16,
    cursor: &Cursor<'_>,
    expected: usize,
) -> Result<(), PortableValueError> {
    if cursor.remaining() != expected {
        return Err(PortableValueError::InvalidPayloadLength {
            tag,
            expected,
            actual: cursor.remaining(),
        });
    }
    Ok(())
}

fn take_collection_prefix(tag: u16, payload: &mut Cursor<'_>) -> Result<usize, PortableValueError> {
    if payload.remaining() < 4 {
        return Err(PortableValueError::Truncated {
            needed: 4,
            remaining: payload.remaining(),
        });
    }
    let count = payload.take_u16()? as usize;
    let reserved = payload.take_u16()?;
    if reserved != 0 {
        return Err(PortableValueError::NonzeroNodeReserved {
            tag,
            found: reserved,
        });
    }
    Ok(count)
}

fn take_identifier(
    payload: &mut Cursor<'_>,
    field: &'static str,
) -> Result<(String, usize), PortableValueError> {
    if payload.remaining() < 4 {
        return Err(PortableValueError::Truncated {
            needed: 4,
            remaining: payload.remaining(),
        });
    }
    let length = payload.take_u16()? as usize;
    let reserved = payload.take_u16()?;
    if reserved != 0 {
        return Err(PortableValueError::NonzeroNodeReserved {
            tag: TAG_TAGGED,
            found: reserved,
        });
    }
    Ok((take_identifier_exact(payload, length, field)?, length))
}

fn take_identifier_exact(
    payload: &mut Cursor<'_>,
    length: usize,
    field: &'static str,
) -> Result<String, PortableValueError> {
    let value = std::str::from_utf8(payload.take(length)?)
        .map_err(|_| PortableValueError::InvalidIdentifier { field })?
        .to_owned();
    validate_identifier(&value, field)?;
    Ok(value)
}

fn take_utf8_remaining<'a>(payload: &mut Cursor<'a>) -> Result<&'a str, PortableValueError> {
    std::str::from_utf8(payload.take_remaining()).map_err(|_| PortableValueError::InvalidUtf8)
}

fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn read_u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], PortableValueError> {
        if self.remaining() < length {
            return Err(PortableValueError::Truncated {
                needed: length,
                remaining: self.remaining(),
            });
        }
        let end = self.offset + length;
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn take_u8(&mut self) -> Result<u8, PortableValueError> {
        Ok(self.take(1)?[0])
    }

    fn take_u16(&mut self) -> Result<u16, PortableValueError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn take_u32(&mut self) -> Result<u32, PortableValueError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn take_remaining(&mut self) -> &'a [u8] {
        let value = &self.bytes[self.offset..];
        self.offset = self.bytes.len();
        value
    }
}

/// Fixed cross-language corpus for the bounded PR4 v1 value ABI.
pub fn world_value_v1_conformance_records() -> Vec<PortableValueRecord> {
    let object = identity_v1_conformance_records()
        .into_iter()
        .find_map(|record| match record {
            IdentityWireRecord::Object(identity) => Some(identity),
            _ => None,
        })
        .expect("identity conformance corpus includes one Object record");
    let code_digest: [u8; 32] = Sha256::digest(b"ostadix-code-a").into();
    let extension_digest: [u8; 32] = Sha256::digest(b"org.ostadix.demo/point/v2").into();

    vec![
        PortableValueRecord::Core(PortableOValue::Null),
        PortableValueRecord::Core(PortableOValue::Bool(false)),
        PortableValueRecord::Core(PortableOValue::Bool(true)),
        PortableValueRecord::Core(PortableOValue::integer(0).unwrap()),
        PortableValueRecord::Core(
            PortableOValue::integer(BigInt::parse_bytes(b"1208925819614629174706177", 10).unwrap())
                .unwrap(),
        ),
        PortableValueRecord::Core(PortableOValue::integer(-42).unwrap()),
        PortableValueRecord::Core(
            PortableOValue::binary_float(FloatFormat::F32, 0x7fc0_1234_u32.to_be_bytes()).unwrap(),
        ),
        PortableValueRecord::Core(
            PortableOValue::binary_float(FloatFormat::F64, 0x8000_0000_0000_0000_u64.to_be_bytes())
                .unwrap(),
        ),
        PortableValueRecord::Core(
            PortableOValue::text(OText {
                utf8: "Ostadix \0 ☃".to_owned(),
                encoding: Some("utf-8".to_owned()),
            })
            .unwrap(),
        ),
        PortableValueRecord::Core(PortableOValue::Char('λ')),
        PortableValueRecord::Core(
            PortableOValue::bytes(OBytes {
                bytes: vec![0, 1, 0xff, 0],
                media_type: Some("application/octet-stream".to_owned()),
            })
            .unwrap(),
        ),
        PortableValueRecord::Core(PortableOValue::List(vec![
            PortableOValue::Null,
            PortableOValue::Bool(true),
            PortableOValue::integer(-7).unwrap(),
        ])),
        PortableValueRecord::Core(
            PortableOValue::record(vec![
                ("zeta".to_owned(), PortableOValue::Bool(false)),
                ("answer".to_owned(), PortableOValue::integer(42).unwrap()),
            ])
            .unwrap(),
        ),
        PortableValueRecord::Core(
            PortableOValue::map(vec![
                (
                    PortableOValue::text(OText {
                        utf8: "one".to_owned(),
                        encoding: None,
                    })
                    .unwrap(),
                    PortableOValue::integer(1).unwrap(),
                ),
                (PortableOValue::Bool(false), PortableOValue::Null),
                (
                    PortableOValue::integer(-1).unwrap(),
                    PortableOValue::Bool(true),
                ),
            ])
            .unwrap(),
        ),
        PortableValueRecord::Core(
            PortableOValue::tagged(
                "some",
                PortableOValue::record(vec![(
                    "value".to_owned(),
                    PortableOValue::integer(9).unwrap(),
                )])
                .unwrap(),
            )
            .unwrap(),
        ),
        PortableValueRecord::Core(PortableOValue::code_ref(code_digest, "o", "main").unwrap()),
        PortableValueRecord::Core(PortableOValue::ObjectRef(object)),
        PortableValueRecord::Core(PortableOValue::error("failed", "bounded failure").unwrap()),
        PortableValueRecord::Extension(
            ExtensionEnvelope::new(
                "org.ostadix.demo",
                "point",
                2,
                extension_digest,
                PortableOValue::record(vec![
                    ("x".to_owned(), PortableOValue::integer(3).unwrap()),
                    ("y".to_owned(), PortableOValue::integer(4).unwrap()),
                ])
                .unwrap(),
            )
            .unwrap(),
        ),
    ]
}

pub fn world_value_v1_conformance_bytes() -> Result<Vec<u8>, PortableValueError> {
    let mut bytes = Vec::new();
    for record in world_value_v1_conformance_records() {
        bytes.extend_from_slice(&record.encode()?);
    }
    Ok(bytes)
}

pub fn world_value_v1_conformance_sha256() -> Result<[u8; 32], PortableValueError> {
    Ok(Sha256::digest(world_value_v1_conformance_bytes()?).into())
}
