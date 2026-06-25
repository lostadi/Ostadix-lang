use std::io::{Read, Write};

use anyhow::{anyhow, bail, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{Map, Number, Value};

const MAX_FRAME_LEN: usize = 128 * 1024 * 1024;

pub(crate) fn write_frame<W, T>(writer: &mut W, message: &T) -> Result<()>
where
    W: Write,
    T: Serialize,
{
    let payload = encode_message(message)?;
    let len: u32 = payload
        .len()
        .try_into()
        .context("wire payload exceeded u32 frame length")?;
    writer
        .write_all(&len.to_be_bytes())
        .context("failed to write wire frame length")?;
    writer
        .write_all(&payload)
        .context("failed to write wire frame payload")?;
    writer.flush().context("failed to flush wire frame")?;
    Ok(())
}

pub(crate) fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: Read,
    T: DeserializeOwned,
{
    let Some(payload) = read_frame_payload(reader)? else {
        return Ok(None);
    };
    decode_message(&payload).map(Some)
}

pub(crate) fn encode_message<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(message).context("failed to lower message to wire value")?;
    let mut out = Vec::new();
    encode_value(&value, &mut out)?;
    Ok(out)
}

pub(crate) fn decode_message<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    let mut decoder = CborDecoder::new(payload);
    let value = decoder.decode_value()?;
    decoder.finish()?;
    serde_json::from_value(value).context("failed to lift wire value into message")
}

fn read_frame_payload<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let mut len_buf = [0_u8; 4];
    let mut read = 0;
    while read < len_buf.len() {
        let n = reader
            .read(&mut len_buf[read..])
            .context("failed to read wire frame length")?;
        if n == 0 {
            if read == 0 {
                return Ok(None);
            }
            bail!("backend process closed stdout in the middle of a wire frame length");
        }
        read += n;
    }

    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_LEN {
        bail!("wire frame length {len} exceeds maximum {MAX_FRAME_LEN}");
    }

    let mut payload = vec![0_u8; len];
    reader
        .read_exact(&mut payload)
        .context("failed to read wire frame payload")?;
    Ok(Some(payload))
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
}

impl<'a> CborDecoder<'a> {
    fn new(payload: &'a [u8]) -> Self {
        Self { payload, offset: 0 }
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
                let len = self.read_len(additional)? as usize;
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
                let len = self.read_len(additional)? as usize;
                let bytes = self.read_bytes(len)?;
                let text = std::str::from_utf8(bytes)
                    .context("wire text string is not valid UTF-8")?
                    .to_string();
                Ok(Value::String(text))
            }
            4 => {
                let len = self.read_len(additional)? as usize;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.decode_value()?);
                }
                Ok(Value::Array(items))
            }
            5 => {
                let len = self.read_len(additional)? as usize;
                let mut map = Map::new();
                for _ in 0..len {
                    let key = self.decode_value()?;
                    let Value::String(key) = key else {
                        bail!("wire map key is not a text string");
                    };
                    let value = self.decode_value()?;
                    map.insert(key, value);
                }
                Ok(Value::Object(map))
            }
            7 => self.decode_simple(additional),
            _ => bail!("unsupported CBOR major type {major} in wire payload"),
        }
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
    use crate::value::{OValue, OWireCommand, OWireResponse};
    use std::collections::HashMap;

    #[test]
    fn wire_command_frame_is_cbor_not_json_lines() {
        let command = OWireCommand::Exec {
            code: "1 + 1".into(),
            bindings: HashMap::from([("x".into(), OValue::int(42))]),
        };
        let mut frame = Vec::new();

        write_frame(&mut frame, &command).unwrap();

        assert_ne!(frame[4], b'{', "wire payload must not be JSON text");
        assert!(
            !frame.ends_with(b"\n"),
            "wire frame must not be line-delimited"
        );
        let decoded: OWireCommand = read_frame(&mut &frame[..]).unwrap().unwrap();
        assert!(matches!(decoded, OWireCommand::Exec { .. }));
    }

    #[test]
    fn wire_response_round_trips_eval_request() {
        let response = OWireResponse::EvalRequest {
            src: "python^(40+2)_python".into(),
            scope: Some(OValue::scope(HashMap::from([(
                "n".into(),
                OValue::int(42),
            )]))),
        };
        let mut frame = Vec::new();

        write_frame(&mut frame, &response).unwrap();

        let decoded: OWireResponse = read_frame(&mut &frame[..]).unwrap().unwrap();
        assert!(matches!(
            decoded,
            OWireResponse::EvalRequest { scope: Some(_), .. }
        ));
    }
}
