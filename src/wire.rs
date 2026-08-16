use std::io::{Read, Write};

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Serialize};

const MAX_FRAME_LEN: usize = 128 * 1024 * 1024;

/// Encode one canonical CBOR message with the O backend frame prefix.
pub fn write_frame<W, T>(writer: &mut W, message: &T) -> Result<()>
where
    W: Write,
    T: Serialize,
{
    let payload = encode_message(message)?;
    write_frame_payload(writer, &payload)
}

pub(crate) fn write_frame_with_max<W, T>(
    writer: &mut W,
    message: &T,
    max_frame_len: usize,
) -> Result<()>
where
    W: Write,
    T: Serialize,
{
    let payload = encode_message(message)?;
    if payload.len() > max_frame_len {
        bail!(
            "wire frame length {} exceeds maximum {max_frame_len}",
            payload.len()
        );
    }
    write_frame_payload(writer, &payload)
}

fn write_frame_payload<W: Write>(writer: &mut W, payload: &[u8]) -> Result<()> {
    let len: u32 = payload
        .len()
        .try_into()
        .context("wire payload exceeded u32 frame length")?;
    writer
        .write_all(&len.to_be_bytes())
        .context("failed to write wire frame length")?;
    writer
        .write_all(payload)
        .context("failed to write wire frame payload")?;
    writer.flush().context("failed to flush wire frame")?;
    Ok(())
}

/// Decode one length-prefixed canonical CBOR backend message.
pub fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: Read,
    T: DeserializeOwned,
{
    read_frame_with_max(reader, MAX_FRAME_LEN)
}

pub(crate) fn read_frame_with_max<R, T>(reader: &mut R, max_frame_len: usize) -> Result<Option<T>>
where
    R: Read,
    T: DeserializeOwned,
{
    let Some(payload) = read_frame_payload(reader, max_frame_len)? else {
        return Ok(None);
    };
    decode_message(&payload).map(Some)
}

pub(crate) fn encode_message<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    crate::canonical_cbor::encode(message)
}

pub(crate) fn decode_message<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    crate::canonical_cbor::decode(payload)
}

fn read_frame_payload<R: Read>(reader: &mut R, max_frame_len: usize) -> Result<Option<Vec<u8>>> {
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
    if len > max_frame_len {
        bail!("wire frame length {len} exceeds maximum {max_frame_len}");
    }

    let mut payload = vec![0_u8; len];
    reader
        .read_exact(&mut payload)
        .context("failed to read wire frame payload")?;
    Ok(Some(payload))
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

    #[test]
    fn caller_frame_limit_is_checked_before_payload_read() {
        let header = 9_u32.to_be_bytes();
        let mut header_only = header.as_slice();

        let error = read_frame_with_max::<_, OWireCommand>(&mut header_only, 8).unwrap_err();

        assert_eq!(error.to_string(), "wire frame length 9 exceeds maximum 8");
        assert!(
            header_only.is_empty(),
            "only the length prefix was consumed"
        );
    }

    #[test]
    fn caller_write_limit_is_checked_before_frame_output() {
        let mut frame = Vec::new();

        let error = write_frame_with_max(&mut frame, &"too large", 1).unwrap_err();

        assert!(error.to_string().contains("exceeds maximum 1"));
        assert!(frame.is_empty());
    }
}
