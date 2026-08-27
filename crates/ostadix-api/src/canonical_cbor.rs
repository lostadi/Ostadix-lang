//! Deterministic CBOR over Ostadix's JSON-compatible data model.
//!
//! This module owns the byte-level canonicalization previously embedded in
//! `wire`.  The wire framing layer and information records deliberately share
//! it so there is one canonical encoder rather than two drifting projections.

use anyhow::{anyhow, bail, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};

const REGISTRY_KEY_ID_DOMAIN_V1: &[u8] = b"OSTADIX/REGISTRY-ED25519-KEY/V1\0";

/// Stable Ostadix Ed25519 public-key identity used by the registry and every
/// protocol that binds one of its key IDs.
pub fn registry_public_key_id(public_key: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(REGISTRY_KEY_ID_DOMAIN_V1);
    hasher.update(public_key);
    hasher.finalize().into()
}

pub(crate) fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(message).context("failed to lower message to wire value")?;
    let mut out = Vec::new();
    encode_value(&value, &mut out)?;
    Ok(out)
}

pub(crate) fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    let mut decoder = CborDecoder::new(payload);
    let value = decoder.decode_value()?;
    decoder.finish()?;
    serde_json::from_value(value).context("failed to lift wire value into message")
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DecodeLimits {
    pub max_bytes: usize,
    pub max_items: usize,
    pub max_depth: usize,
}

pub(crate) fn decode_bounded<T: DeserializeOwned>(
    payload: &[u8],
    limits: DecodeLimits,
) -> Result<T> {
    if payload.len() > limits.max_bytes {
        bail!(
            "canonical payload has {} bytes; bounded maximum is {}",
            payload.len(),
            limits.max_bytes
        );
    }
    let mut decoder = CborDecoder::new_bounded(payload, limits);
    let value = decoder.decode_value()?;
    decoder.finish()?;
    serde_json::from_value(value).context("failed to lift wire value into message")
}

/// Builds the domain-separated preimage used by canonical signed records.
///
/// The byte formula is exactly `domain || u64_be(body.len()) || body`.
pub(crate) fn signing_preimage(domain: &[u8], body: &[u8]) -> Result<Vec<u8>> {
    let body_len = u64::try_from(body.len()).context("canonical signing body is too large")?;
    let capacity = domain
        .len()
        .checked_add(8)
        .and_then(|size| size.checked_add(body.len()))
        .context("canonical signing preimage is too large")?;
    let mut preimage = Vec::with_capacity(capacity);
    preimage.extend_from_slice(domain);
    preimage.extend_from_slice(&body_len.to_be_bytes());
    preimage.extend_from_slice(body);
    Ok(preimage)
}

fn encode_value(value: &Value, out: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Null => out.push(0xf6),
        Value::Bool(false) => out.push(0xf4),
        Value::Bool(true) => out.push(0xf5),
        Value::Number(number) => encode_number(number, out)?,
        Value::String(text) => encode_text(text, out)?,
        Value::Array(items) => {
            encode_type_len(4, items.len() as u64, out);
            for item in items {
                encode_value(item, out)?;
            }
        }
        Value::Object(map) => {
            let mut entries = map
                .iter()
                .map(|(key, value)| {
                    let mut encoded_key = Vec::new();
                    encode_text(key, &mut encoded_key)?;
                    let mut encoded_value = Vec::new();
                    encode_value(value, &mut encoded_value)?;
                    Ok((encoded_key, encoded_value))
                })
                .collect::<Result<Vec<_>>>()?;
            entries.sort_by(|(left, _), (right, _)| {
                left.len().cmp(&right.len()).then_with(|| left.cmp(right))
            });

            encode_type_len(5, entries.len() as u64, out);
            for (key, value) in entries {
                out.extend_from_slice(&key);
                out.extend_from_slice(&value);
            }
        }
    }
    Ok(())
}

fn encode_number(number: &Number, out: &mut Vec<u8>) -> Result<()> {
    if let Some(value) = number.as_u64() {
        encode_type_len(0, value, out);
    } else if let Some(value) = number.as_i64() {
        if value >= 0 {
            encode_type_len(0, value as u64, out);
        } else {
            encode_type_len(1, (-1_i128 - value as i128) as u64, out);
        }
    } else if let Some(value) = number.as_f64() {
        out.push(0xfb);
        out.extend_from_slice(&value.to_bits().to_be_bytes());
    } else {
        bail!("unsupported JSON number in wire value: {number}");
    }
    Ok(())
}

fn encode_text(text: &str, out: &mut Vec<u8>) -> Result<()> {
    encode_type_len(3, text.len() as u64, out);
    out.extend_from_slice(text.as_bytes());
    Ok(())
}

fn encode_type_len(major: u8, len: u64, out: &mut Vec<u8>) {
    let major = major << 5;
    match len {
        0..=23 => out.push(major | len as u8),
        24..=0xff => out.extend_from_slice(&[major | 24, len as u8]),
        0x100..=0xffff => {
            out.push(major | 25);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(major | 26);
            out.extend_from_slice(&(len as u32).to_be_bytes());
        }
        _ => {
            out.push(major | 27);
            out.extend_from_slice(&len.to_be_bytes());
        }
    }
}

struct CborDecoder<'a> {
    payload: &'a [u8],
    offset: usize,
    limits: Option<DecodeLimits>,
    decoded_items: usize,
}

impl<'a> CborDecoder<'a> {
    fn new(payload: &'a [u8]) -> Self {
        Self {
            payload,
            offset: 0,
            limits: None,
            decoded_items: 0,
        }
    }

    fn new_bounded(payload: &'a [u8], limits: DecodeLimits) -> Self {
        Self {
            payload,
            offset: 0,
            limits: Some(limits),
            decoded_items: 0,
        }
    }

    fn finish(&self) -> Result<()> {
        if self.offset == self.payload.len() {
            Ok(())
        } else {
            bail!(
                "wire payload has {} trailing bytes",
                self.payload.len() - self.offset
            )
        }
    }

    fn decode_value(&mut self) -> Result<Value> {
        self.decode_value_at_depth(0)
    }

    fn decode_value_at_depth(&mut self, depth: usize) -> Result<Value> {
        self.note_item(depth)?;
        let initial = self.read_u8()?;
        let major = initial >> 5;
        let additional = initial & 0x1f;
        match major {
            0 => Ok(Value::Number(Number::from(self.read_len(additional)?))),
            1 => {
                let raw = self.read_len(additional)?;
                let value = -1_i128 - raw as i128;
                let value: i64 = value
                    .try_into()
                    .context("negative integer is outside JSON-compatible i64 range")?;
                Ok(Value::Number(Number::from(value)))
            }
            2 => {
                let len = self.read_sized_len(additional, "byte string", 1)?;
                self.reserve_embedded_items(len)?;
                let bytes = self.read_bytes(len)?;
                Ok(Value::Array(
                    bytes
                        .iter()
                        .copied()
                        .map(|byte| Value::Number(Number::from(byte)))
                        .collect(),
                ))
            }
            3 => {
                let len = self.read_sized_len(additional, "text string", 1)?;
                let bytes = self.read_bytes(len)?;
                let text = std::str::from_utf8(bytes)
                    .context("wire text string is not valid UTF-8")?
                    .to_string();
                Ok(Value::String(text))
            }
            4 => {
                let len = self.read_sized_len(additional, "array", 1)?;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.decode_value_at_depth(depth.saturating_add(1))?);
                }
                Ok(Value::Array(items))
            }
            5 => {
                let len = self.read_sized_len(additional, "map", 2)?;
                let mut map = Map::new();
                for _ in 0..len {
                    let key = self.decode_value_at_depth(depth.saturating_add(1))?;
                    let Value::String(key) = key else {
                        bail!("wire map key is not a text string");
                    };
                    let value = self.decode_value_at_depth(depth.saturating_add(1))?;
                    map.insert(key, value);
                }
                Ok(Value::Object(map))
            }
            7 => self.decode_simple(additional),
            _ => bail!("unsupported CBOR major type {major} in wire payload"),
        }
    }

    fn note_item(&mut self, depth: usize) -> Result<()> {
        let Some(limits) = self.limits else {
            return Ok(());
        };
        if depth > limits.max_depth {
            bail!(
                "canonical payload nesting depth {depth} exceeds bounded maximum {}",
                limits.max_depth
            );
        }
        self.decoded_items = self
            .decoded_items
            .checked_add(1)
            .context("canonical payload item counter overflow")?;
        if self.decoded_items > limits.max_items {
            bail!(
                "canonical payload item count exceeds bounded maximum {}",
                limits.max_items
            );
        }
        Ok(())
    }

    fn read_sized_len(
        &mut self,
        additional: u8,
        kind: &str,
        minimum_bytes_per_item: usize,
    ) -> Result<usize> {
        let raw = self.read_len(additional)?;
        let len = usize::try_from(raw)
            .with_context(|| format!("{kind} length is outside the host usize range"))?;
        let remaining = self.payload.len().saturating_sub(self.offset);
        let maximum_from_input = remaining / minimum_bytes_per_item;
        if len > maximum_from_input {
            bail!(
                "canonical {kind} declares {len} items/bytes but at most {maximum_from_input} fit in the remaining payload"
            );
        }
        if let Some(limits) = self.limits {
            let remaining_items = limits.max_items.saturating_sub(self.decoded_items);
            if len > remaining_items {
                bail!(
                    "canonical {kind} length {len} exceeds the bounded remaining item budget {remaining_items}"
                );
            }
        }
        Ok(len)
    }

    fn reserve_embedded_items(&mut self, count: usize) -> Result<()> {
        let Some(limits) = self.limits else {
            return Ok(());
        };
        self.decoded_items = self
            .decoded_items
            .checked_add(count)
            .context("canonical payload item counter overflow")?;
        if self.decoded_items > limits.max_items {
            bail!(
                "canonical payload item count exceeds bounded maximum {}",
                limits.max_items
            );
        }
        Ok(())
    }

    fn decode_simple(&mut self, additional: u8) -> Result<Value> {
        match additional {
            20 => Ok(Value::Bool(false)),
            21 => Ok(Value::Bool(true)),
            22 => Ok(Value::Null),
            26 => {
                let mut bytes = [0_u8; 4];
                bytes.copy_from_slice(self.read_bytes(4)?);
                let value = f32::from_bits(u32::from_be_bytes(bytes)) as f64;
                let number = Number::from_f64(value).ok_or_else(|| anyhow!("non-finite f32"))?;
                Ok(Value::Number(number))
            }
            27 => {
                let mut bytes = [0_u8; 8];
                bytes.copy_from_slice(self.read_bytes(8)?);
                let value = f64::from_bits(u64::from_be_bytes(bytes));
                let number = Number::from_f64(value).ok_or_else(|| anyhow!("non-finite f64"))?;
                Ok(Value::Number(number))
            }
            other => bail!("unsupported CBOR simple value {other} in wire payload"),
        }
    }

    fn read_len(&mut self, additional: u8) -> Result<u64> {
        match additional {
            value @ 0..=23 => Ok(value as u64),
            24 => Ok(self.read_u8()? as u64),
            25 => Ok(u16::from_be_bytes(self.read_array()?) as u64),
            26 => Ok(u32::from_be_bytes(self.read_array()?) as u64),
            27 => Ok(u64::from_be_bytes(self.read_array()?)),
            31 => bail!("indefinite-length CBOR is not allowed on the O wire"),
            other => bail!("invalid CBOR length discriminator {other}"),
        }
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut bytes = [0_u8; N];
        bytes.copy_from_slice(self.read_bytes(N)?);
        Ok(bytes)
    }

    fn read_u8(&mut self) -> Result<u8> {
        let Some(byte) = self.payload.get(self.offset).copied() else {
            bail!("unexpected end of wire payload");
        };
        self.offset += 1;
        Ok(byte)
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .context("wire payload offset overflow")?;
        if end > self.payload.len() {
            bail!("unexpected end of wire payload");
        }
        let bytes = &self.payload[self.offset..end];
        self.offset = end;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct Fixture {
        z: u64,
        aa: String,
        a: bool,
    }

    #[test]
    fn canonical_map_order_and_round_trip_are_stable() {
        let fixture = Fixture {
            z: 42,
            aa: "value".to_string(),
            a: true,
        };
        let first = encode(&fixture).unwrap();
        let second = encode(&fixture).unwrap();
        assert_eq!(first, second);
        assert_eq!(decode::<Fixture>(&first).unwrap(), fixture);
    }

    #[test]
    fn insertion_order_does_not_change_bytes() {
        let left = BTreeMap::from([("aa", 2_u64), ("b", 1)]);
        let right = BTreeMap::from([("b", 1_u64), ("aa", 2)]);
        assert_eq!(encode(&left).unwrap(), encode(&right).unwrap());
    }

    #[test]
    fn signing_preimage_known_answer_pins_domain_length_and_body() {
        let preimage =
            signing_preimage(b"OSTADIX/HOSTED-JOURNAL/V2\0", &[0xa1, 0x61, 0x78, 0x01]).unwrap();

        assert_eq!(
            hex::encode(preimage),
            "4f5354414449582f484f535445442d4a4f55524e414c2f5632000000000000000004a1617801"
        );
    }

    #[test]
    fn registry_public_key_identity_remains_domain_separated() {
        assert_eq!(
            hex::encode(registry_public_key_id(&[0x42; 32])),
            "c17f43f677080cb8f77403e60826cd0b00c516e05038837f673b316fbcd46361"
        );
    }
}
