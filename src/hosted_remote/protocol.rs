//! Versioned, bounded messages for the hosted-placement preview.
//!
//! These messages intentionally carry one prepared O source operation. They
//! are not project bundles, World mutations, migration checkpoints, or a
//! generic command-execution protocol.

use std::io::{Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ir::BackendRegistry;
use crate::value::OValue;

pub const HOSTED_PROTOCOL_V1: &str = "ostadix.hosted-transport/v1";
pub const NODE_PROFILE_SCHEMA_V1: &str = "ostadix.node-profile/v1";
pub const NODE_DOCTOR_SCHEMA_V1: &str = "ostadix.node-doctor/v1";
pub const REMOTE_PREPARED_OPERATION_SCHEMA_V1: &str = "ostadix.remote-prepared-operation/v1";
pub const HOSTED_OPERATION_RECEIPT_SCHEMA_V1: &str = "ostadix.hosted-operation-receipt/v1";

/// Hard payload ceiling for every request and response (the four-byte frame
/// prefix is additional). The generic backend wire protocol has a larger
/// compatibility ceiling; hosted placement deliberately narrows it here.
pub const MAX_HOSTED_FRAME_BYTES: usize = 2 * 1024 * 1024;
/// Largest source document accepted in one prepared operation.
pub const MAX_HOSTED_SOURCE_BYTES: usize = 1024 * 1024;
/// Largest serialized successful value a caller may request.
pub const MAX_HOSTED_OUTPUT_BYTES: usize = 768 * 1024;
pub const MAX_HOSTED_ID_BYTES: usize = 128;
pub const MAX_HOSTED_ERROR_BYTES: usize = 8 * 1024;

/// Encode a message with Ostadix's deterministic CBOR encoder and the hosted
/// transport's smaller frame bound.
pub fn write_hosted_frame<W, T>(writer: &mut W, message: &T) -> Result<()>
where
    W: Write,
    T: Serialize,
{
    crate::wire::write_frame_with_max(writer, message, MAX_HOSTED_FRAME_BYTES)
}

/// Decode one hosted frame. EOF before a new frame is `Ok(None)`; truncated or
/// oversized frames fail closed.
pub fn read_hosted_frame<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: Read,
    T: DeserializeOwned + Serialize,
{
    let mut len_bytes = [0_u8; 4];
    let mut read = 0;
    while read < len_bytes.len() {
        let count = reader
            .read(&mut len_bytes[read..])
            .context("failed to read hosted frame length")?;
        if count == 0 {
            if read == 0 {
                return Ok(None);
            }
            bail!("connection closed in the middle of a hosted frame length");
        }
        read += count;
    }
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len > MAX_HOSTED_FRAME_BYTES {
        bail!("hosted frame length {len} exceeds maximum {MAX_HOSTED_FRAME_BYTES}");
    }
    let mut payload = vec![0_u8; len];
    reader
        .read_exact(&mut payload)
        .context("connection closed in the middle of a hosted frame payload")?;
    let decoded: T = crate::wire::decode_message(&payload)?;
    let canonical = crate::wire::encode_message(&decoded)?;
    if canonical != payload {
        bail!("hosted frame payload is valid CBOR but not canonical Ostadix CBOR");
    }
    Ok(Some(decoded))
}

/// Canonical bytes used by protocol digests. This is intentionally a wrapper
/// rather than making the crate-internal wire primitives public globally.
pub fn canonical_hosted_bytes<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    let bytes = crate::wire::encode_message(message)?;
    if bytes.len() > MAX_HOSTED_FRAME_BYTES {
        bail!(
            "canonical hosted message length {} exceeds maximum {}",
            bytes.len(),
            MAX_HOSTED_FRAME_BYTES
        );
    }
    Ok(bytes)
}

pub fn canonical_hosted_sha256<T: Serialize>(message: &T) -> Result<String> {
    Ok(hex::encode(Sha256::digest(canonical_hosted_bytes(
        message,
    )?)))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn unix_time_ms() -> Result<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis();
    millis
        .try_into()
        .context("Unix timestamp exceeds the hosted protocol's u64 range")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeBackendCatalogEntryV1 {
    pub canonical_name: String,
    pub aliases: Vec<String>,
    pub specification_sha256: String,
}

/// Descriptive facts advertised by one node. Catalog entries describe
/// adapters compiled into this runtime; they are not probes, health evidence,
/// leases, or permission to execute them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeProfileV1 {
    pub schema: String,
    pub protocol: String,
    pub node_id: String,
    pub transport: String,
    pub backend_catalog_sha256: String,
    pub catalogued_backends: Vec<NodeBackendCatalogEntryV1>,
    pub max_frame_bytes: u64,
    pub max_source_bytes: u64,
    pub max_output_bytes: u64,
    pub max_concurrent_connections: u32,
    pub execution_isolation: String,
}

impl NodeProfileV1 {
    pub fn local(node_id: impl Into<String>, max_concurrent_connections: usize) -> Result<Self> {
        let registry = BackendRegistry::global();
        let catalogued_backends = registry
            .canonical_specs()
            .iter()
            .map(|spec| {
                Ok(NodeBackendCatalogEntryV1 {
                    canonical_name: spec.name.to_string(),
                    aliases: spec.aliases.iter().map(|alias| alias.to_string()).collect(),
                    specification_sha256: registry.specification_sha256(spec.name).with_context(
                        || format!("catalog omitted specification digest for `{}`", spec.name),
                    )?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let profile = Self {
            schema: NODE_PROFILE_SCHEMA_V1.to_string(),
            protocol: HOSTED_PROTOCOL_V1.to_string(),
            node_id: node_id.into(),
            transport: "tcp+tls1.3+mutual-x509".to_string(),
            backend_catalog_sha256: registry.catalog_sha256(),
            catalogued_backends,
            max_frame_bytes: MAX_HOSTED_FRAME_BYTES as u64,
            max_source_bytes: MAX_HOSTED_SOURCE_BYTES as u64,
            max_output_bytes: MAX_HOSTED_OUTPUT_BYTES as u64,
            max_concurrent_connections: max_concurrent_connections
                .try_into()
                .context("node connection limit exceeds u32")?,
            execution_isolation: "fresh-evaluator-per-operation".to_string(),
        };
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != NODE_PROFILE_SCHEMA_V1 {
            bail!("unsupported node profile schema `{}`", self.schema);
        }
        if self.protocol != HOSTED_PROTOCOL_V1 {
            bail!("unsupported hosted protocol `{}`", self.protocol);
        }
        validate_identifier("node_id", &self.node_id)?;
        validate_sha256("backend_catalog_sha256", &self.backend_catalog_sha256)?;
        if self.transport != "tcp+tls1.3+mutual-x509" {
            bail!(
                "node profile advertises unsupported transport `{}`",
                self.transport
            );
        }
        if self.max_frame_bytes != MAX_HOSTED_FRAME_BYTES as u64
            || self.max_source_bytes > MAX_HOSTED_SOURCE_BYTES as u64
            || self.max_output_bytes > MAX_HOSTED_OUTPUT_BYTES as u64
            || self.max_concurrent_connections == 0
        {
            bail!("node profile advertises invalid hosted transport limits");
        }
        for backend in &self.catalogued_backends {
            if backend.canonical_name.is_empty() {
                bail!("node profile contains an empty backend name");
            }
            validate_sha256(
                "backend specification digest",
                &backend.specification_sha256,
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDoctorV1 {
    pub schema: String,
    pub node_id: String,
    pub ready: bool,
    pub backend_catalog_sha256: String,
    pub profile_sha256: String,
    pub shim_directory: String,
    pub checks: Vec<NodeDoctorCheckV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDoctorCheckV1 {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// A single source operation prepared by a client. `deadline_unix_ms` is an
/// absolute latest-completion boundary. The node checks it before evaluation
/// and suppresses a late result after evaluation; current `Evaluator` APIs do
/// not provide safe asynchronous cancellation of already-running effects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemotePreparedOperationV1 {
    pub schema: String,
    pub task_id: String,
    pub attempt_id: String,
    pub source_utf8: String,
    pub source_sha256: String,
    pub expected_backend_catalog_sha256: String,
    pub deadline_unix_ms: u64,
    pub output_limit_bytes: u64,
}

impl RemotePreparedOperationV1 {
    pub fn new(
        task_id: impl Into<String>,
        attempt_id: impl Into<String>,
        source_utf8: impl Into<String>,
        expected_backend_catalog_sha256: impl Into<String>,
        deadline_unix_ms: u64,
        output_limit_bytes: u64,
    ) -> Result<Self> {
        let source_utf8 = source_utf8.into();
        let operation = Self {
            schema: REMOTE_PREPARED_OPERATION_SCHEMA_V1.to_string(),
            task_id: task_id.into(),
            attempt_id: attempt_id.into(),
            source_sha256: sha256_hex(source_utf8.as_bytes()),
            source_utf8,
            expected_backend_catalog_sha256: expected_backend_catalog_sha256.into(),
            deadline_unix_ms,
            output_limit_bytes,
        };
        operation.validate_structure()?;
        Ok(operation)
    }

    /// Validate bounded syntax without consulting mutable node state or time.
    pub fn validate_structure(&self) -> Result<()> {
        if self.schema != REMOTE_PREPARED_OPERATION_SCHEMA_V1 {
            bail!("unsupported prepared-operation schema `{}`", self.schema);
        }
        validate_identifier("task_id", &self.task_id)?;
        validate_identifier("attempt_id", &self.attempt_id)?;
        if self.source_utf8.len() > MAX_HOSTED_SOURCE_BYTES {
            bail!(
                "prepared source length {} exceeds maximum {}",
                self.source_utf8.len(),
                MAX_HOSTED_SOURCE_BYTES
            );
        }
        validate_sha256("source_sha256", &self.source_sha256)?;
        validate_sha256(
            "expected_backend_catalog_sha256",
            &self.expected_backend_catalog_sha256,
        )?;
        if self.deadline_unix_ms == 0 {
            bail!("prepared operation requires a non-zero absolute deadline");
        }
        if self.output_limit_bytes == 0 || self.output_limit_bytes > MAX_HOSTED_OUTPUT_BYTES as u64
        {
            bail!(
                "output limit {} must be between 1 and {} bytes",
                self.output_limit_bytes,
                MAX_HOSTED_OUTPUT_BYTES
            );
        }
        Ok(())
    }

    pub fn operation_sha256(&self) -> Result<String> {
        canonical_hosted_sha256(self)
    }
}

// Preserve the V1 public outcome API. Boxing the successful value would only
// move this versioned schema's size trade-off into every producer and consumer.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HostedOperationOutcomeV1 {
    Succeeded {
        value: OValue,
    },
    Failed {
        stage: HostedFailureStageV1,
        code: String,
        message: String,
    },
}

impl HostedOperationOutcomeV1 {
    pub fn failed(
        stage: HostedFailureStageV1,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let mut message = message.into();
        if message.len() > MAX_HOSTED_ERROR_BYTES {
            message.truncate(MAX_HOSTED_ERROR_BYTES);
            message.push_str(" [truncated]");
        }
        Self::Failed {
            stage,
            code: code.into(),
            message,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedFailureStageV1 {
    Admission,
    Parse,
    Evaluate,
    Output,
    Deadline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostedOperationReceiptV1 {
    pub schema: String,
    pub node_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub operation_sha256: String,
    pub source_sha256: String,
    pub backend_catalog_sha256: String,
    pub started_unix_ms: u64,
    pub finished_unix_ms: u64,
    pub outcome: HostedOperationOutcomeV1,
    pub outcome_sha256: String,
    pub receipt_sha256: String,
}

#[derive(Serialize)]
struct ReceiptDigestMaterialV1<'a> {
    schema: &'a str,
    node_id: &'a str,
    task_id: &'a str,
    attempt_id: &'a str,
    operation_sha256: &'a str,
    source_sha256: &'a str,
    backend_catalog_sha256: &'a str,
    started_unix_ms: u64,
    finished_unix_ms: u64,
    outcome: &'a HostedOperationOutcomeV1,
    outcome_sha256: &'a str,
}

impl HostedOperationReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        node_id: impl Into<String>,
        operation: &RemotePreparedOperationV1,
        actual_source_sha256: impl Into<String>,
        backend_catalog_sha256: impl Into<String>,
        started_unix_ms: u64,
        finished_unix_ms: u64,
        outcome: HostedOperationOutcomeV1,
    ) -> Result<Self> {
        let outcome_sha256 = canonical_hosted_sha256(&outcome)?;
        let mut receipt = Self {
            schema: HOSTED_OPERATION_RECEIPT_SCHEMA_V1.to_string(),
            node_id: node_id.into(),
            task_id: operation.task_id.clone(),
            attempt_id: operation.attempt_id.clone(),
            operation_sha256: operation.operation_sha256()?,
            source_sha256: actual_source_sha256.into(),
            backend_catalog_sha256: backend_catalog_sha256.into(),
            started_unix_ms,
            finished_unix_ms,
            outcome,
            outcome_sha256,
            receipt_sha256: String::new(),
        };
        receipt.receipt_sha256 = receipt.compute_receipt_sha256()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != HOSTED_OPERATION_RECEIPT_SCHEMA_V1 {
            bail!("unsupported hosted receipt schema `{}`", self.schema);
        }
        validate_identifier("node_id", &self.node_id)?;
        validate_identifier("task_id", &self.task_id)?;
        validate_identifier("attempt_id", &self.attempt_id)?;
        validate_sha256("operation_sha256", &self.operation_sha256)?;
        validate_sha256("source_sha256", &self.source_sha256)?;
        validate_sha256("backend_catalog_sha256", &self.backend_catalog_sha256)?;
        validate_sha256("outcome_sha256", &self.outcome_sha256)?;
        validate_sha256("receipt_sha256", &self.receipt_sha256)?;
        if self.finished_unix_ms < self.started_unix_ms {
            bail!("hosted receipt finishes before it starts");
        }
        let actual_outcome = canonical_hosted_sha256(&self.outcome)?;
        if actual_outcome != self.outcome_sha256 {
            bail!("hosted receipt outcome digest mismatch");
        }
        let actual_receipt = self.compute_receipt_sha256()?;
        if actual_receipt != self.receipt_sha256 {
            bail!("hosted receipt digest mismatch");
        }
        Ok(())
    }

    fn compute_receipt_sha256(&self) -> Result<String> {
        canonical_hosted_sha256(&ReceiptDigestMaterialV1 {
            schema: &self.schema,
            node_id: &self.node_id,
            task_id: &self.task_id,
            attempt_id: &self.attempt_id,
            operation_sha256: &self.operation_sha256,
            source_sha256: &self.source_sha256,
            backend_catalog_sha256: &self.backend_catalog_sha256,
            started_unix_ms: self.started_unix_ms,
            finished_unix_ms: self.finished_unix_ms,
            outcome: &self.outcome,
            outcome_sha256: &self.outcome_sha256,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum HostedRequestV1 {
    Profile {
        protocol: String,
    },
    Doctor {
        protocol: String,
    },
    Run {
        protocol: String,
        operation: RemotePreparedOperationV1,
    },
}

impl HostedRequestV1 {
    pub fn profile() -> Self {
        Self::Profile {
            protocol: HOSTED_PROTOCOL_V1.to_string(),
        }
    }

    pub fn doctor() -> Self {
        Self::Doctor {
            protocol: HOSTED_PROTOCOL_V1.to_string(),
        }
    }

    pub fn run(operation: RemotePreparedOperationV1) -> Self {
        Self::Run {
            protocol: HOSTED_PROTOCOL_V1.to_string(),
            operation,
        }
    }

    pub fn validate(&self) -> Result<()> {
        let protocol = match self {
            Self::Profile { protocol } | Self::Doctor { protocol } | Self::Run { protocol, .. } => {
                protocol
            }
        };
        if protocol != HOSTED_PROTOCOL_V1 {
            bail!("unsupported hosted protocol `{protocol}`");
        }
        if let Self::Run { operation, .. } = self {
            operation.validate_structure()?;
        }
        Ok(())
    }
}

// Preserve direct construction of the frozen V1 response variants; the wire
// boundary is intentionally kept distinct from an internal boxed transport.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum HostedResponseV1 {
    Profile { profile: NodeProfileV1 },
    Doctor { doctor: NodeDoctorV1 },
    Run { receipt: HostedOperationReceiptV1 },
    Error { error: HostedProtocolErrorV1 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedProtocolErrorV1 {
    pub code: String,
    pub message: String,
}

impl HostedProtocolErrorV1 {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > MAX_HOSTED_ERROR_BYTES {
            message.truncate(MAX_HOSTED_ERROR_BYTES);
            message.push_str(" [truncated]");
        }
        Self {
            code: code.into(),
            message,
        }
    }
}

fn validate_identifier(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_HOSTED_ID_BYTES {
        bail!("{field} length must be between 1 and {MAX_HOSTED_ID_BYTES} bytes");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        bail!("{field} contains characters outside [A-Za-z0-9._:-]");
    }
    Ok(())
}

fn validate_sha256(field: &str, value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{field} must be a lowercase 64-character SHA-256 digest");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn profile_is_catalog_bound_and_explicitly_descriptive() {
        let profile = NodeProfileV1::local("node-a", 4).unwrap();
        profile.validate().unwrap();
        assert_eq!(
            profile.backend_catalog_sha256,
            BackendRegistry::global().catalog_sha256()
        );
        assert_eq!(profile.execution_isolation, "fresh-evaluator-per-operation");
        assert!(!profile.catalogued_backends.is_empty());
    }

    #[test]
    fn prepared_operation_hashes_exact_source_bytes() {
        let source = "python^[*]($: print('ok'))";
        let operation =
            RemotePreparedOperationV1::new("task-1", "attempt-1", source, "a".repeat(64), 1, 1024)
                .unwrap();
        assert_eq!(operation.source_sha256, sha256_hex(source.as_bytes()));
        operation.validate_structure().unwrap();

        let mut changed = operation;
        changed.source_utf8.push(' ');
        // Structural validation deliberately does not trust mutable node time
        // or state; execution performs this semantic re-hash.
        assert_ne!(
            changed.source_sha256,
            sha256_hex(changed.source_utf8.as_bytes())
        );
    }

    #[test]
    fn hosted_frame_round_trip_uses_bounded_wire_codec() {
        let request = HostedRequestV1::profile();
        let mut bytes = Vec::new();
        write_hosted_frame(&mut bytes, &request).unwrap();
        let decoded: HostedRequestV1 = read_hosted_frame(&mut Cursor::new(bytes)).unwrap().unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn hosted_reader_rejects_oversized_and_noncanonical_payloads() {
        let oversized = ((MAX_HOSTED_FRAME_BYTES + 1) as u32).to_be_bytes().to_vec();
        let error =
            read_hosted_frame::<_, HostedRequestV1>(&mut Cursor::new(oversized)).unwrap_err();
        assert!(error.to_string().contains("exceeds maximum"));

        let canonical = canonical_hosted_bytes(&HostedRequestV1::profile()).unwrap();
        assert_eq!(canonical[0] >> 5, 5, "request must encode as a CBOR map");
        let map_len = canonical[0] & 0x1f;
        assert!(map_len < 24);
        let mut noncanonical_payload = vec![0xb8, map_len];
        noncanonical_payload.extend_from_slice(&canonical[1..]);
        let mut framed = (noncanonical_payload.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(&noncanonical_payload);
        let error = read_hosted_frame::<_, HostedRequestV1>(&mut Cursor::new(framed)).unwrap_err();
        assert!(error.to_string().contains("not canonical"));
    }

    #[test]
    fn receipt_digest_detects_outcome_mutation() {
        let operation =
            RemotePreparedOperationV1::new("task-1", "attempt-1", "2", "b".repeat(64), 1, 1024)
                .unwrap();
        let mut receipt = HostedOperationReceiptV1::issue(
            "node-a",
            &operation,
            operation.source_sha256.clone(),
            "b".repeat(64),
            10,
            11,
            HostedOperationOutcomeV1::Succeeded {
                value: OValue::int(2),
            },
        )
        .unwrap();
        receipt.validate().unwrap();
        receipt.outcome = HostedOperationOutcomeV1::Succeeded {
            value: OValue::int(3),
        };
        assert!(receipt.validate().is_err());
    }
}
