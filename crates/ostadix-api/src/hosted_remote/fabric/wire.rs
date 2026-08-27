//! Bounded transport framing for authenticated execution-Fabric messages.
//!
//! Each message has exactly two independently length-prefixed records:
//!
//! ```text
//! u32_be header_len | canonical-CBOR header | u32_be payload_len | exact M2 payload
//! ```
//!
//! `payload_len == 0` is the sole encoding of an absent payload. The M3a
//! request/response codecs decide which variants require a payload and verify
//! its exact declared length and digest. This module owns framing and I/O only;
//! it does not authenticate, authorize, execute, publish, or settle work.
//! Fabric V1 permits exactly one frame in each TLS write direction. The sender
//! follows its frame with TLS `close_notify`; the receiver requires clean EOF
//! after the declared payload before the message is usable. This rejects a
//! second frame, a delayed suffix, raw TCP EOF, and trailing bytes inside either
//! canonical record without relying on a timing-sensitive socket peek.

use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

use crate::execution_fabric::{MAX_EXECUTION_CANDIDATE_BYTES, MAX_EXECUTION_CAPSULE_BYTES};
use crate::execution_fabric_authority::{
    decode_fabric_request_v1, decode_fabric_response_v1, encode_fabric_request_v1,
    encode_fabric_response_v1, FabricEncodedMessageV1, FabricRequestV1, FabricResponseV1,
    MAX_FABRIC_HEADER_BYTES,
};

use super::super::tls::{HostedClientStream, HostedServerStream};

pub const FABRIC_LENGTH_PREFIX_BYTES_V1: usize = std::mem::size_of::<u32>();
pub const MAX_FABRIC_REQUEST_PAYLOAD_BYTES_V1: usize = MAX_EXECUTION_CAPSULE_BYTES;
pub const MAX_FABRIC_RESPONSE_PAYLOAD_BYTES_V1: usize = MAX_EXECUTION_CANDIDATE_BYTES;

/// Encode and write one complete Fabric request frame.
pub(crate) fn write_fabric_request_v1<W: Write>(
    writer: &mut W,
    request: &FabricRequestV1,
) -> Result<()> {
    let encoded = encode_fabric_request_v1(request)
        .map_err(anyhow::Error::new)
        .context("failed to encode Fabric request")?;
    write_encoded_message(
        writer,
        &encoded,
        MAX_FABRIC_REQUEST_PAYLOAD_BYTES_V1,
        "request",
    )
}

/// Read the only Fabric request frame and require clean end-of-stream.
///
/// Clean EOF before the header-length prefix is `Ok(None)`. Any partial frame
/// or any byte after the declared payload fails closed.
pub(crate) fn read_fabric_request_v1<R: Read>(reader: &mut R) -> Result<Option<FabricRequestV1>> {
    let Some((header, payload)) =
        read_encoded_message(reader, MAX_FABRIC_REQUEST_PAYLOAD_BYTES_V1, "request")?
    else {
        return Ok(None);
    };
    let request = decode_fabric_request_v1(&header, payload.as_deref())
        .map_err(anyhow::Error::new)
        .context("failed to decode Fabric request")?;
    require_clean_message_eof(reader, "request")?;
    Ok(Some(request))
}

/// Encode and write one complete Fabric response frame.
pub(crate) fn write_fabric_response_v1<W: Write>(
    writer: &mut W,
    response: &FabricResponseV1,
) -> Result<()> {
    let encoded = encode_fabric_response_v1(response)
        .map_err(anyhow::Error::new)
        .context("failed to encode Fabric response")?;
    write_encoded_message(
        writer,
        &encoded,
        MAX_FABRIC_RESPONSE_PAYLOAD_BYTES_V1,
        "response",
    )
}

/// Write already validated, persisted response bytes without decode/re-encode.
///
/// Durable duplicate handling uses this boundary to return the exact terminal
/// header and candidate bytes stored by the provider ledger. Bounds and the
/// absent-versus-empty payload rule are rechecked before any byte is written.
pub(crate) fn write_fabric_encoded_response_parts_v1<W: Write>(
    writer: &mut W,
    header_bytes: &[u8],
    payload_bytes: Option<&[u8]>,
) -> Result<()> {
    write_message_parts(
        writer,
        header_bytes,
        payload_bytes,
        MAX_FABRIC_RESPONSE_PAYLOAD_BYTES_V1,
        "response",
    )
}

/// Read the only Fabric response frame and require clean end-of-stream.
///
/// Clean EOF before the header-length prefix is `Ok(None)`. Any partial frame
/// or any byte after the declared payload fails closed.
pub(crate) fn read_fabric_response_v1<R: Read>(reader: &mut R) -> Result<Option<FabricResponseV1>> {
    let Some((header, payload)) =
        read_encoded_message(reader, MAX_FABRIC_RESPONSE_PAYLOAD_BYTES_V1, "response")?
    else {
        return Ok(None);
    };
    let response = decode_fabric_response_v1(&header, payload.as_deref())
        .map_err(anyhow::Error::new)
        .context("failed to decode Fabric response")?;
    require_clean_message_eof(reader, "response")?;
    Ok(Some(response))
}

/// Set the write deadline on a mutually authenticated Fabric client stream,
/// write one request, and authenticate the end of the request direction with
/// TLS `close_notify`. Zero timeouts are rejected rather than interpreted as
/// platform-specific blocking behavior.
pub fn write_fabric_client_request_v1(
    stream: &mut HostedClientStream,
    request: &FabricRequestV1,
    timeout: Duration,
) -> Result<()> {
    set_write_timeout(&stream.sock, timeout)?;
    write_fabric_request_v1(stream, request)?;
    stream.conn.send_close_notify();
    flush_phase(stream, "Fabric request TLS close-notify")
}

/// Set the read deadline on a mutually authenticated Fabric client stream,
/// then require one response followed by authenticated TLS end-of-stream. A
/// clean close before the response is an error.
pub fn read_fabric_client_response_v1(
    stream: &mut HostedClientStream,
    timeout: Duration,
) -> Result<FabricResponseV1> {
    set_read_timeout(&stream.sock, timeout)?;
    read_fabric_response_v1(stream)?.context("Fabric peer closed before returning a response")
}

/// Set the read deadline on a mutually authenticated Fabric server stream,
/// then read its only request and require authenticated TLS end-of-stream.
/// Clean EOF before a request remains a normal connection close.
pub(crate) fn read_fabric_server_request_v1(
    stream: &mut HostedServerStream,
    timeout: Duration,
) -> Result<Option<FabricRequestV1>> {
    set_read_timeout(&stream.sock, timeout)?;
    read_fabric_request_v1(stream)
}

/// Set the write deadline on a mutually authenticated Fabric server stream,
/// write one response, and authenticate the end of the response direction with
/// TLS `close_notify`.
pub(crate) fn write_fabric_server_response_v1(
    stream: &mut HostedServerStream,
    response: &FabricResponseV1,
    timeout: Duration,
) -> Result<()> {
    set_write_timeout(&stream.sock, timeout)?;
    write_fabric_response_v1(stream, response)?;
    finish_fabric_server_write_v1(stream)
}

pub(crate) fn write_fabric_server_encoded_response_parts_v1(
    stream: &mut HostedServerStream,
    header_bytes: &[u8],
    payload_bytes: Option<&[u8]>,
    timeout: Duration,
) -> Result<()> {
    set_write_timeout(&stream.sock, timeout)?;
    write_fabric_encoded_response_parts_v1(stream, header_bytes, payload_bytes)?;
    finish_fabric_server_write_v1(stream)
}

fn finish_fabric_server_write_v1(stream: &mut HostedServerStream) -> Result<()> {
    stream.conn.send_close_notify();
    flush_phase(stream, "Fabric response TLS close-notify")
}

fn write_encoded_message<W: Write>(
    writer: &mut W,
    encoded: &FabricEncodedMessageV1,
    maximum_payload: usize,
    kind: &'static str,
) -> Result<()> {
    write_message_parts(
        writer,
        encoded.header_bytes(),
        encoded.payload_bytes(),
        maximum_payload,
        kind,
    )
}

fn write_message_parts<W: Write>(
    writer: &mut W,
    header: &[u8],
    payload: Option<&[u8]>,
    maximum_payload: usize,
    kind: &'static str,
) -> Result<()> {
    if header.is_empty() || header.len() > MAX_FABRIC_HEADER_BYTES {
        bail!(
            "Fabric {kind} header length {} is outside 1..={MAX_FABRIC_HEADER_BYTES}",
            header.len()
        );
    }
    if payload.is_some_and(<[u8]>::is_empty) {
        bail!("Fabric {kind} payload must be absent rather than present and empty");
    }
    let payload_len = payload.map_or(0, <[u8]>::len);
    if payload_len > maximum_payload {
        bail!("Fabric {kind} payload length {payload_len} exceeds maximum {maximum_payload}");
    }
    let header_len =
        u32::try_from(header.len()).context("Fabric header length does not fit u32")?;
    let payload_len =
        u32::try_from(payload_len).context("Fabric payload length does not fit u32")?;
    let total = FABRIC_LENGTH_PREFIX_BYTES_V1
        .checked_add(header.len())
        .and_then(|value| value.checked_add(FABRIC_LENGTH_PREFIX_BYTES_V1))
        .and_then(|value| value.checked_add(payload_len as usize))
        .context("Fabric frame length overflow")?;
    let mut frame = Vec::with_capacity(total);
    frame.extend_from_slice(&header_len.to_be_bytes());
    frame.extend_from_slice(header);
    frame.extend_from_slice(&payload_len.to_be_bytes());
    if let Some(payload) = payload {
        frame.extend_from_slice(payload);
    }
    write_all_phase(writer, &frame, "Fabric frame")?;
    flush_phase(writer, "Fabric frame")
}

fn read_encoded_message<R: Read>(
    reader: &mut R,
    maximum_payload: usize,
    kind: &'static str,
) -> Result<Option<(Vec<u8>, Option<Vec<u8>>)>> {
    let Some(header_len) = read_optional_u32(reader, "Fabric header length")? else {
        return Ok(None);
    };
    let header_len = header_len as usize;
    if header_len == 0 || header_len > MAX_FABRIC_HEADER_BYTES {
        bail!("Fabric {kind} header length {header_len} is outside 1..={MAX_FABRIC_HEADER_BYTES}");
    }
    let mut header = vec![0_u8; header_len];
    read_exact_phase(reader, &mut header, "Fabric header")?;

    let payload_len = read_required_u32(reader, "Fabric payload length")? as usize;
    if payload_len > maximum_payload {
        bail!("Fabric {kind} payload length {payload_len} exceeds maximum {maximum_payload}");
    }
    let payload = if payload_len == 0 {
        None
    } else {
        let mut payload = vec![0_u8; payload_len];
        read_exact_phase(reader, &mut payload, "Fabric payload")?;
        Some(payload)
    };
    Ok(Some((header, payload)))
}

fn require_clean_message_eof<R: Read>(reader: &mut R, kind: &'static str) -> Result<()> {
    let mut trailing = [0_u8; 1];
    loop {
        match reader.read(&mut trailing) {
            Ok(0) => return Ok(()),
            Ok(_) => bail!("Fabric {kind} contains trailing bytes after its only frame"),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(io_error(
                    if kind == "request" {
                        "Fabric request end-of-stream"
                    } else {
                        "Fabric response end-of-stream"
                    },
                    "reading",
                    error,
                ))
            }
        }
    }
}

fn read_optional_u32<R: Read>(reader: &mut R, phase: &'static str) -> Result<Option<u32>> {
    let mut bytes = [0_u8; FABRIC_LENGTH_PREFIX_BYTES_V1];
    let mut filled = 0;
    while filled < bytes.len() {
        match reader.read(&mut bytes[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => bail!("connection closed in the middle of {phase}"),
            Ok(count) => filled += count,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(io_error(phase, "reading", error)),
        }
    }
    Ok(Some(u32::from_be_bytes(bytes)))
}

fn read_required_u32<R: Read>(reader: &mut R, phase: &'static str) -> Result<u32> {
    let mut bytes = [0_u8; FABRIC_LENGTH_PREFIX_BYTES_V1];
    read_exact_phase(reader, &mut bytes, phase)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_exact_phase<R: Read>(
    reader: &mut R,
    mut destination: &mut [u8],
    phase: &'static str,
) -> Result<()> {
    while !destination.is_empty() {
        match reader.read(destination) {
            Ok(0) => bail!("connection closed before a complete {phase}"),
            Ok(count) => destination = &mut destination[count..],
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(io_error(phase, "reading", error)),
        }
    }
    Ok(())
}

fn write_all_phase<W: Write>(writer: &mut W, mut source: &[u8], phase: &'static str) -> Result<()> {
    while !source.is_empty() {
        match writer.write(source) {
            Ok(0) => {
                return Err(anyhow!(io::Error::new(
                    ErrorKind::WriteZero,
                    format!("failed to write a complete {phase}"),
                )))
            }
            Ok(count) => source = &source[count..],
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(io_error(phase, "writing", error)),
        }
    }
    Ok(())
}

fn flush_phase<W: Write>(writer: &mut W, phase: &'static str) -> Result<()> {
    writer
        .flush()
        .map_err(|error| io_error(phase, "flushing", error))
}

fn io_error(phase: &'static str, action: &'static str, error: io::Error) -> anyhow::Error {
    if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) {
        anyhow!("timed out while {action} {phase}: {error}")
    } else {
        anyhow!("failed while {action} {phase}: {error}")
    }
}

fn set_read_timeout(stream: &TcpStream, timeout: Duration) -> Result<()> {
    validate_timeout(timeout)?;
    stream
        .set_read_timeout(Some(timeout))
        .context("failed to set Fabric read timeout")
}

fn set_write_timeout(stream: &TcpStream, timeout: Duration) -> Result<()> {
    validate_timeout(timeout)?;
    stream
        .set_write_timeout(Some(timeout))
        .context("failed to set Fabric write timeout")
}

fn validate_timeout(timeout: Duration) -> Result<()> {
    if timeout.is_zero() {
        bail!("Fabric I/O timeout must be nonzero");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor, ErrorKind, Read, Write};

    use crate::execution_fabric::{
        encode_execution_candidate_v1, encode_execution_capsule_v1, AttemptIdV1,
        CandidateOutcomeV1, CandidateOutputV1, ExecutionCandidateV1, ExecutionCapsuleV1,
        ExecutionIdV1, ExecutionLimitsV1, InputBindingV1, InputManifestV1, LogicalTaskIdV1,
        OutputContractV1, OutputFidelityV1, OutputValueKindV1, RendererPartV1, Sha256DigestV1,
        SourceClosedRendererV1, TrustedInlineRendererV1,
    };
    use crate::execution_fabric_authority::{
        ExecutionCellIncarnationV1, FabricAttemptQueryV1, FabricAttemptStatusV1,
        FabricSigningKeyV1, FabricSourceClosureV1, FabricSubmissionV1, FabricTargetBindingV1,
        FabricTerminalCandidateV1, PlacementLeaseV3, FABRIC_SOURCE_CLOSURE_DIALECT_V1,
        FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1,
    };
    use crate::placement_protocol::{GenerationV1, SemanticDigestV1, UnixMillisV1};
    use crate::value::OText;
    use crate::world::{PortableOValue, PortableValueRecord, MAX_OVALUE_RECORD_BYTES};

    use super::*;

    const DEADLINE_UNIX_MS: u64 = 2_000_000_000_000;

    struct Fixture {
        submission: FabricSubmissionV1,
        terminal: FabricTerminalCandidateV1,
    }

    fn digest(seed: u8) -> Sha256DigestV1 {
        [seed; 32]
    }

    fn semantic_digest(seed: u8) -> SemanticDigestV1 {
        SemanticDigestV1::from_sha256(hex::encode(digest(seed))).unwrap()
    }

    fn fixture() -> Fixture {
        let execution = ExecutionIdV1::new(digest(1)).unwrap();
        let task = LogicalTaskIdV1::new(execution, digest(2)).unwrap();
        let attempt = AttemptIdV1::new(task, 1).unwrap();
        let input = PortableValueRecord::Core(
            PortableOValue::text(OText {
                utf8: "world".to_string(),
                encoding: Some("utf-8".to_string()),
            })
            .unwrap(),
        );
        let inputs =
            InputManifestV1::new(vec![InputBindingV1::new("name", &input).unwrap()]).unwrap();
        let region = SourceClosedRendererV1::new(
            TrustedInlineRendererV1::Text,
            vec![
                RendererPartV1::literal("hello "),
                RendererPartV1::input("name"),
            ],
            digest(3),
            digest(4),
            digest(5),
            digest(6),
        )
        .unwrap();
        let output = OutputContractV1::for_renderer(
            "result",
            TrustedInlineRendererV1::Text,
            MAX_OVALUE_RECORD_BYTES,
        )
        .unwrap();
        let capsule = ExecutionCapsuleV1::new(
            attempt,
            region,
            digest(7),
            inputs,
            output,
            DEADLINE_UNIX_MS,
            ExecutionLimitsV1::new(30_000, 16 * 1024, MAX_OVALUE_RECORD_BYTES).unwrap(),
        )
        .unwrap();
        let capsule_bytes = encode_execution_capsule_v1(&capsule).unwrap();
        let source_closure = FabricSourceClosureV1::new(
            FABRIC_SOURCE_CLOSURE_DIALECT_V1,
            "main = render(name)",
            FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1,
            "eager",
            digest(10),
            digest(3),
            digest(4),
        )
        .unwrap();
        let authority_key = FabricSigningKeyV1::from_secret_bytes([0x11; 32]);
        let target = FabricTargetBindingV1::new(
            semantic_digest(20),
            "fabric-node-a",
            GenerationV1::new(7).unwrap(),
            ExecutionCellIncarnationV1::new(11).unwrap(),
            semantic_digest(21),
            GenerationV1::new(8).unwrap(),
            GenerationV1::new(9).unwrap(),
            semantic_digest(22),
            semantic_digest(23),
            semantic_digest(24),
            semantic_digest(25),
            semantic_digest(26),
            semantic_digest(27),
            semantic_digest(28),
        )
        .unwrap();
        let lease = PlacementLeaseV3::new(
            authority_key.key_id_digest(),
            semantic_digest(29),
            target,
            &source_closure,
            &capsule,
            UnixMillisV1::new(DEADLINE_UNIX_MS - 30_000),
            UnixMillisV1::new(DEADLINE_UNIX_MS),
        )
        .unwrap();
        let submission = FabricSubmissionV1::new(
            authority_key.sign_execution_lease(lease).unwrap(),
            source_closure,
            capsule_bytes,
        )
        .unwrap();

        let candidate_output = PortableValueRecord::Core(
            PortableOValue::text(OText {
                utf8: "hello world".to_string(),
                encoding: Some("utf-8".to_string()),
            })
            .unwrap(),
        );
        let candidate = ExecutionCandidateV1::new(
            &capsule,
            CandidateOutcomeV1::Succeeded {
                output: CandidateOutputV1::new(
                    "result",
                    &candidate_output,
                    OutputValueKindV1::Text,
                    OutputFidelityV1::Structural,
                )
                .unwrap(),
            },
            DEADLINE_UNIX_MS - 1,
        )
        .unwrap();
        let candidate_bytes = encode_execution_candidate_v1(&candidate).unwrap();
        let terminal = FabricSigningKeyV1::from_secret_bytes([0x22; 32])
            .sign_terminal_candidate(&submission, candidate_bytes, 25)
            .unwrap();
        Fixture {
            submission,
            terminal,
        }
    }

    fn payload_prefix_offset(frame: &[u8]) -> usize {
        let header_len = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
        FABRIC_LENGTH_PREFIX_BYTES_V1 + header_len
    }

    #[test]
    fn request_and_response_round_trip_with_exact_big_endian_lengths() {
        let fixture = fixture();
        let request = FabricRequestV1::SubmitPureAttempt(fixture.submission.clone());
        let encoded_request = encode_fabric_request_v1(&request).unwrap();
        let mut request_frame = Vec::new();
        write_fabric_request_v1(&mut request_frame, &request).unwrap();
        assert_eq!(
            u32::from_be_bytes(request_frame[..4].try_into().unwrap()) as usize,
            encoded_request.header_bytes().len()
        );
        let request_payload_offset = payload_prefix_offset(&request_frame);
        assert_eq!(
            u32::from_be_bytes(
                request_frame[request_payload_offset..request_payload_offset + 4]
                    .try_into()
                    .unwrap()
            ) as usize,
            encoded_request.payload_bytes().unwrap().len()
        );
        assert_eq!(
            read_fabric_request_v1(&mut Cursor::new(request_frame))
                .unwrap()
                .unwrap(),
            request
        );

        let response = FabricResponseV1::TerminalCandidate(fixture.terminal);
        let encoded_response = encode_fabric_response_v1(&response).unwrap();
        let mut response_frame = Vec::new();
        write_fabric_response_v1(&mut response_frame, &response).unwrap();
        assert_eq!(
            u32::from_be_bytes(response_frame[..4].try_into().unwrap()) as usize,
            encoded_response.header_bytes().len()
        );
        let response_payload_offset = payload_prefix_offset(&response_frame);
        assert_eq!(
            u32::from_be_bytes(
                response_frame[response_payload_offset..response_payload_offset + 4]
                    .try_into()
                    .unwrap()
            ) as usize,
            encoded_response.payload_bytes().unwrap().len()
        );
        assert_eq!(
            read_fabric_response_v1(&mut Cursor::new(response_frame))
                .unwrap()
                .unwrap(),
            response
        );
    }

    #[test]
    fn persisted_response_parts_write_the_exact_frame_and_recheck_bounds() {
        let fixture = fixture();
        let response = FabricResponseV1::TerminalCandidate(fixture.terminal);
        let encoded = encode_fabric_response_v1(&response).unwrap();

        let mut ordinary_frame = Vec::new();
        write_fabric_response_v1(&mut ordinary_frame, &response).unwrap();
        let mut persisted_frame = Vec::new();
        write_fabric_encoded_response_parts_v1(
            &mut persisted_frame,
            encoded.header_bytes(),
            encoded.payload_bytes(),
        )
        .unwrap();
        assert_eq!(persisted_frame, ordinary_frame);

        assert!(
            write_fabric_encoded_response_parts_v1(&mut Vec::new(), &[], None)
                .unwrap_err()
                .to_string()
                .contains("header length")
        );
        assert!(write_fabric_encoded_response_parts_v1(
            &mut Vec::new(),
            &vec![0; MAX_FABRIC_HEADER_BYTES + 1],
            None,
        )
        .unwrap_err()
        .to_string()
        .contains("header length"));
        assert!(write_fabric_encoded_response_parts_v1(
            &mut Vec::new(),
            encoded.header_bytes(),
            Some(&[]),
        )
        .unwrap_err()
        .to_string()
        .contains("absent rather than present and empty"));
        assert!(write_fabric_encoded_response_parts_v1(
            &mut Vec::new(),
            encoded.header_bytes(),
            Some(&vec![0; MAX_FABRIC_RESPONSE_PAYLOAD_BYTES_V1 + 1]),
        )
        .unwrap_err()
        .to_string()
        .contains("payload length"));
    }

    #[test]
    fn zero_payload_length_is_only_absence_and_variant_rules_remain_exact() {
        let fixture = fixture();
        let query = FabricRequestV1::QueryAttempt(FabricAttemptQueryV1::from_submission(
            &fixture.submission,
        ));
        let mut query_frame = Vec::new();
        write_fabric_request_v1(&mut query_frame, &query).unwrap();
        let query_payload_offset = payload_prefix_offset(&query_frame);
        assert_eq!(
            &query_frame[query_payload_offset..query_payload_offset + 4],
            &[0, 0, 0, 0]
        );
        assert_eq!(
            read_fabric_request_v1(&mut Cursor::new(query_frame.clone()))
                .unwrap()
                .unwrap(),
            query
        );

        query_frame[query_payload_offset..query_payload_offset + 4]
            .copy_from_slice(&1_u32.to_be_bytes());
        query_frame.push(0);
        let error = read_fabric_request_v1(&mut Cursor::new(query_frame)).unwrap_err();
        assert!(format!("{error:#}").contains("unexpected payload"));

        let submission = FabricRequestV1::SubmitPureAttempt(fixture.submission.clone());
        let mut submission_frame = Vec::new();
        write_fabric_request_v1(&mut submission_frame, &submission).unwrap();
        let submission_payload_offset = payload_prefix_offset(&submission_frame);
        submission_frame.truncate(submission_payload_offset + 4);
        submission_frame[submission_payload_offset..submission_payload_offset + 4]
            .copy_from_slice(&0_u32.to_be_bytes());
        let error = read_fabric_request_v1(&mut Cursor::new(submission_frame)).unwrap_err();
        assert!(format!("{error:#}").contains("omitted its capsule payload"));

        let accepted =
            FabricResponseV1::Accepted(FabricAttemptStatusV1::from_submission(&fixture.submission));
        let mut accepted_frame = Vec::new();
        write_fabric_response_v1(&mut accepted_frame, &accepted).unwrap();
        let accepted_payload_offset = payload_prefix_offset(&accepted_frame);
        assert_eq!(
            &accepted_frame[accepted_payload_offset..accepted_payload_offset + 4],
            &[0, 0, 0, 0]
        );
        accepted_frame[accepted_payload_offset..accepted_payload_offset + 4]
            .copy_from_slice(&1_u32.to_be_bytes());
        accepted_frame.push(0);
        let error = read_fabric_response_v1(&mut Cursor::new(accepted_frame)).unwrap_err();
        assert!(format!("{error:#}").contains("unexpected payload"));
    }

    #[test]
    fn hostile_lengths_and_truncations_fail_before_unbounded_allocation() {
        assert!(read_fabric_request_v1(&mut Cursor::new(vec![0, 0]))
            .unwrap_err()
            .to_string()
            .contains("middle of Fabric header length"));
        assert!(
            read_fabric_request_v1(&mut Cursor::new(0_u32.to_be_bytes()))
                .unwrap_err()
                .to_string()
                .contains("outside")
        );
        assert!(read_fabric_request_v1(&mut Cursor::new(
            ((MAX_FABRIC_HEADER_BYTES + 1) as u32).to_be_bytes()
        ))
        .unwrap_err()
        .to_string()
        .contains("header length"));

        let fixture = fixture();
        let request = FabricRequestV1::SubmitPureAttempt(fixture.submission);
        let mut frame = Vec::new();
        write_fabric_request_v1(&mut frame, &request).unwrap();
        let payload_offset = payload_prefix_offset(&frame);

        assert!(
            read_fabric_request_v1(&mut Cursor::new(frame[..payload_offset - 1].to_vec()))
                .unwrap_err()
                .to_string()
                .contains("complete Fabric header")
        );
        assert!(
            read_fabric_request_v1(&mut Cursor::new(frame[..payload_offset + 3].to_vec()))
                .unwrap_err()
                .to_string()
                .contains("complete Fabric payload length")
        );
        assert!(
            read_fabric_request_v1(&mut Cursor::new(frame[..frame.len() - 1].to_vec()))
                .unwrap_err()
                .to_string()
                .contains("complete Fabric payload")
        );

        let mut oversized_payload_prefix = frame[..payload_offset].to_vec();
        oversized_payload_prefix
            .extend_from_slice(&((MAX_FABRIC_REQUEST_PAYLOAD_BYTES_V1 + 1) as u32).to_be_bytes());
        assert!(
            read_fabric_request_v1(&mut Cursor::new(oversized_payload_prefix))
                .unwrap_err()
                .to_string()
                .contains("payload length")
        );
    }

    struct TimeoutReader;

    impl Read for TimeoutReader {
        fn read(&mut self, _destination: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(ErrorKind::TimedOut, "fixture timeout"))
        }
    }

    struct TimeoutWriter;

    impl Write for TimeoutWriter {
        fn write(&mut self, _source: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(ErrorKind::WouldBlock, "fixture timeout"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn timeout_errors_remain_distinguishable_from_malformed_frames() {
        assert!(read_fabric_request_v1(&mut TimeoutReader)
            .unwrap_err()
            .to_string()
            .contains("timed out while reading Fabric header length"));

        let fixture = fixture();
        let request = FabricRequestV1::SubmitPureAttempt(fixture.submission);
        assert!(write_fabric_request_v1(&mut TimeoutWriter, &request)
            .unwrap_err()
            .to_string()
            .contains("timed out while writing Fabric frame"));
    }

    #[test]
    fn request_and_response_reject_bytes_after_their_only_frame() {
        let fixture = fixture();
        let request = FabricRequestV1::SubmitPureAttempt(fixture.submission);
        let mut request_frame = Vec::new();
        write_fabric_request_v1(&mut request_frame, &request).unwrap();

        let mut request_with_suffix = request_frame.clone();
        request_with_suffix.push(0x7f);
        assert!(
            read_fabric_request_v1(&mut Cursor::new(request_with_suffix))
                .unwrap_err()
                .to_string()
                .contains("trailing bytes")
        );

        let mut two_requests = request_frame.clone();
        two_requests.extend_from_slice(&request_frame);
        assert!(read_fabric_request_v1(&mut Cursor::new(two_requests))
            .unwrap_err()
            .to_string()
            .contains("trailing bytes"));

        let response = FabricResponseV1::TerminalCandidate(fixture.terminal);
        let mut response_frame = Vec::new();
        write_fabric_response_v1(&mut response_frame, &response).unwrap();

        let mut response_with_suffix = response_frame.clone();
        response_with_suffix.push(0x7f);
        assert!(
            read_fabric_response_v1(&mut Cursor::new(response_with_suffix))
                .unwrap_err()
                .to_string()
                .contains("trailing bytes")
        );

        let mut two_responses = response_frame.clone();
        two_responses.extend_from_slice(&response_frame);
        assert!(read_fabric_response_v1(&mut Cursor::new(two_responses))
            .unwrap_err()
            .to_string()
            .contains("trailing bytes"));
    }

    #[test]
    fn message_completion_requires_eof_not_temporary_absence() {
        let fixture = fixture();
        let request = FabricRequestV1::SubmitPureAttempt(fixture.submission);
        let mut request_frame = Vec::new();
        write_fabric_request_v1(&mut request_frame, &request).unwrap();
        let mut reader = Cursor::new(request_frame).chain(TimeoutReader);

        assert!(read_fabric_request_v1(&mut reader)
            .unwrap_err()
            .to_string()
            .contains("timed out while reading Fabric request end-of-stream"));
    }

    #[test]
    fn clean_eof_before_the_only_message_is_not_a_truncated_frame() {
        assert!(read_fabric_request_v1(&mut Cursor::new(Vec::<u8>::new()))
            .unwrap()
            .is_none());
        assert!(read_fabric_response_v1(&mut Cursor::new(Vec::<u8>::new()))
            .unwrap()
            .is_none());
    }
}
