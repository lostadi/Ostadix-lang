//! Versioned process protocol for hosted native-live runtime packages.
//!
//! This is a Stage-1 reference surface. Runtime workers are ordinary child
//! processes and exchange only framed canonical-CBOR messages carrying
//! `OValue`s. They are not O-core processes and this module does not claim the
//! kernel isolation required by the native live-system milestone.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::live_system::manifest;
use crate::value::OValue;
use crate::wire;

pub const RUNTIME_PROGRAM_SCHEMA: &str = "ocore.runtime-program/v1";
pub const RUNTIME_PROTOCOL: &str = "ocore.runtime-service/v1";
pub(crate) const RUNTIME_MAX_FRAME_LEN: usize = 1024 * 1024;
const RUNTIME_MAX_PROGRAM_BYTES: usize = 1024 * 1024;
const RUNTIME_MAX_OPERATIONS: usize = 256;
const RUNTIME_MAX_OPERATION_MESSAGE_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProgram {
    pub schema: String,
    pub world: String,
    pub health: RuntimeHealth,
    #[serde(default)]
    pub operations: BTreeMap<String, OperationSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeHealth {
    pub status: HealthStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Healthy,
    Unhealthy,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationSpec {
    Identity,
    Wrap {
        field: String,
    },
    Prefix {
        text: String,
    },
    IntPair {
        lhs: i64,
        rhs: i64,
    },
    SumFields {
        lhs: String,
        rhs: String,
    },
    Fail {
        message: String,
    },
    /// Hosted test-runtime primitive used to exercise supervisor recovery.
    Crash {},
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
// Keeping OValue inline makes the typed request API direct; the protocol's
// explicit 1 MiB frame ceiling bounds the serialized representation.
#[allow(clippy::large_enum_variant)]
pub(crate) enum RuntimeRequest {
    Health {
        nonce: u64,
    },
    Invoke {
        service: String,
        operation: String,
        input: OValue,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
// See RuntimeRequest: the fixed wire ceiling is the relevant resource bound.
#[allow(clippy::large_enum_variant)]
pub(crate) enum RuntimeResponse {
    Healthy { nonce: u64, world: String },
    Unhealthy { nonce: u64, message: String },
    Result { value: OValue },
    Error { message: String },
    Stopped,
}

impl RuntimeProgram {
    pub fn parse(text: &str) -> Result<Self> {
        enforce_runtime_program_size(text.len() as u64)?;
        let program: Self = toml::from_str(text).context("invalid runtime program TOML")?;
        program.validate()?;
        Ok(program)
    }

    fn validate(&self) -> Result<()> {
        if self.schema != RUNTIME_PROGRAM_SCHEMA {
            bail!(
                "unsupported runtime program schema `{}`; expected `{RUNTIME_PROGRAM_SCHEMA}`",
                self.schema
            );
        }
        validate_identifier("runtime world", &self.world)?;
        if self.operations.is_empty() {
            bail!("runtime program must declare at least one operation");
        }
        if self.operations.len() > RUNTIME_MAX_OPERATIONS {
            bail!(
                "runtime program declares {} operations; maximum is {RUNTIME_MAX_OPERATIONS}",
                self.operations.len()
            );
        }
        for (name, operation) in &self.operations {
            validate_identifier("operation", name)?;
            match operation {
                OperationSpec::Wrap { field } => validate_identifier("wrap field", field)?,
                OperationSpec::Prefix { text }
                    if text.len() > RUNTIME_MAX_OPERATION_MESSAGE_BYTES =>
                {
                    bail!(
                        "prefix operation text exceeds {RUNTIME_MAX_OPERATION_MESSAGE_BYTES} bytes"
                    )
                }
                OperationSpec::SumFields { lhs, rhs } => {
                    validate_identifier("sum lhs field", lhs)?;
                    validate_identifier("sum rhs field", rhs)?;
                    if lhs == rhs {
                        bail!("sum operation requires two distinct fields");
                    }
                }
                OperationSpec::Fail { message } => {
                    if message.is_empty() {
                        bail!("fail operation requires a non-empty message");
                    }
                    if message.len() > RUNTIME_MAX_OPERATION_MESSAGE_BYTES {
                        bail!(
                            "fail operation message exceeds {RUNTIME_MAX_OPERATION_MESSAGE_BYTES} bytes"
                        );
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn invoke(&self, operation: &str, input: OValue) -> Result<OValue> {
        let operation = self
            .operations
            .get(operation)
            .with_context(|| format!("runtime operation `{operation}` is not registered"))?;
        match operation {
            OperationSpec::Identity => Ok(input),
            OperationSpec::Wrap { field } => Ok(OValue::Object {
                fields: BTreeMap::from([
                    ("world".into(), OValue::str_(self.world.clone())),
                    (field.clone(), input),
                ]),
            }),
            OperationSpec::Prefix { text } => match input {
                OValue::Text { v } => Ok(OValue::str_(format!("{text}{}", v.utf8))),
                other => bail!(
                    "prefix operation requires str input, got {}",
                    other.type_name()
                ),
            },
            OperationSpec::IntPair { lhs, rhs } => Ok(OValue::Object {
                fields: BTreeMap::from([
                    ("lhs".into(), OValue::int(*lhs)),
                    ("rhs".into(), OValue::int(*rhs)),
                    ("world".into(), OValue::str_(self.world.clone())),
                ]),
            }),
            OperationSpec::SumFields { lhs, rhs } => {
                let fields = match input {
                    OValue::Object { fields } => fields,
                    other => bail!(
                        "sum_fields operation requires object input, got {}",
                        other.type_name()
                    ),
                };
                let left = fields
                    .get(lhs)
                    .with_context(|| format!("sum input is missing `{lhs}`"))?
                    .as_int()?;
                let right = fields
                    .get(rhs)
                    .with_context(|| format!("sum input is missing `{rhs}`"))?
                    .as_int()?;
                let result = left
                    .checked_add(right)
                    .context("sum_fields result overflowed i64")?;
                Ok(OValue::int(result))
            }
            OperationSpec::Fail { message } => bail!("{message}"),
            OperationSpec::Crash {} => {
                bail!("crash operation is available only through the hosted worker protocol")
            }
        }
    }
}

fn validate_identifier(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 255
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("{label} `{value}` is not a bounded portable identifier");
    }
    Ok(())
}

fn enforce_runtime_program_size(size: u64) -> Result<()> {
    if size > RUNTIME_MAX_PROGRAM_BYTES as u64 {
        bail!("runtime program exceeds {RUNTIME_MAX_PROGRAM_BYTES}-byte limit (got {size})");
    }
    Ok(())
}

fn read_runtime_program(file: File, display_path: &Path) -> Result<String> {
    let metadata = file.metadata().with_context(|| {
        format!(
            "failed to inspect opened runtime entry {}",
            display_path.display()
        )
    })?;
    if !metadata.is_file() {
        bail!("runtime entry must be a regular non-symlink file");
    }
    enforce_runtime_program_size(metadata.len())?;

    let mut bounded = file.take(RUNTIME_MAX_PROGRAM_BYTES as u64 + 1);
    let mut text = String::with_capacity(metadata.len() as usize);
    bounded
        .read_to_string(&mut text)
        .with_context(|| format!("failed to read runtime entry {}", display_path.display()))?;
    enforce_runtime_program_size(text.len() as u64)?;
    let file = bounded.into_inner();
    let final_metadata = file.metadata().with_context(|| {
        format!(
            "failed to reinspect opened runtime entry {}",
            display_path.display()
        )
    })?;
    if final_metadata.len() != metadata.len() || text.len() as u64 != metadata.len() {
        bail!(
            "runtime entry {} changed while it was being read",
            display_path.display()
        );
    }
    Ok(text)
}

fn checked_entry(package_root: &Path, entry: &str) -> Result<(File, PathBuf)> {
    let relative = manifest::runtime_entry_payload_path(entry)
        .with_context(|| format!("invalid package-internal runtime entry `{entry}`"))?;
    let display_path = package_root.join(&relative);
    let file = manifest::open_payload_regular_file(package_root, &relative).with_context(|| {
        format!(
            "failed to securely open runtime entry {}",
            display_path.display()
        )
    })?;
    Ok((file, display_path))
}

pub fn worker_main(package_root: &Path, entry: &str, service: &str) -> Result<()> {
    validate_identifier("service", service)?;
    let (entry, display_path) = checked_entry(package_root, entry)?;
    // Parse the bytes from the descriptor that passed no-follow traversal.
    // Never canonicalize and reopen the pathname after verification.
    let text = read_runtime_program(entry, &display_path)?;
    let program = RuntimeProgram::parse(&text)?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve(&program, service, &mut stdin.lock(), &mut stdout.lock())
}

fn serve<R: Read, W: Write>(
    program: &RuntimeProgram,
    service: &str,
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    while let Some(request) =
        wire::read_frame_with_max::<_, RuntimeRequest>(reader, RUNTIME_MAX_FRAME_LEN)?
    {
        let response = match request {
            RuntimeRequest::Health { nonce } => match program.health.status {
                HealthStatus::Healthy => RuntimeResponse::Healthy {
                    nonce,
                    world: program.world.clone(),
                },
                HealthStatus::Unhealthy => RuntimeResponse::Unhealthy {
                    nonce,
                    message: format!("runtime world `{}` failed its health policy", program.world),
                },
            },
            RuntimeRequest::Invoke {
                service: requested,
                operation,
                input,
            } => {
                if requested != service {
                    RuntimeResponse::Error {
                        message: format!(
                            "runtime worker is scoped to service `{service}`, not `{requested}`"
                        ),
                    }
                } else {
                    if matches!(
                        program.operations.get(&operation),
                        Some(OperationSpec::Crash {})
                    ) {
                        bail!("hosted test worker crash requested by operation `{operation}`");
                    }
                    match program.invoke(&operation, input) {
                        Ok(value) => RuntimeResponse::Result { value },
                        Err(error) => RuntimeResponse::Error {
                            message: error.to_string(),
                        },
                    }
                }
            }
            RuntimeRequest::Shutdown => {
                wire::write_frame_with_max(
                    writer,
                    &RuntimeResponse::Stopped,
                    RUNTIME_MAX_FRAME_LEN,
                )?;
                return Ok(());
            }
        };
        wire::write_frame_with_max(writer, &response, RUNTIME_MAX_FRAME_LEN)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn program() -> RuntimeProgram {
        RuntimeProgram::parse(
            r#"
schema = "ocore.runtime-program/v1"
world = "native.alpha"

[health]
status = "healthy"

[operations.compute]
kind = "wrap"
field = "input"
"#,
        )
        .unwrap()
    }

    fn identity_program_with_operations(count: usize) -> String {
        let mut source = format!(
            "schema = \"{RUNTIME_PROGRAM_SCHEMA}\"\nworld = \"native.limits\"\n\n[health]\nstatus = \"healthy\"\n"
        );
        for index in 0..count {
            source.push_str(&format!("\n[operations.op{index}]\nkind = \"identity\"\n"));
        }
        source
    }

    fn program_with_message_operation(kind: &str, field: &str, value: &str) -> String {
        format!(
            "schema = \"{RUNTIME_PROGRAM_SCHEMA}\"\nworld = \"native.limits\"\n\n[health]\nstatus = \"healthy\"\n\n[operations.test]\nkind = \"{kind}\"\n{field} = \"{value}\"\n"
        )
    }

    #[test]
    fn runtime_program_is_strict_and_wraps_ovalue() {
        assert!(RuntimeProgram::parse(
            r#"
schema = "ocore.runtime-program/v1"
world = "native.alpha"
unknown = true
[health]
status = "healthy"
[operations.compute]
kind = "identity"
"#
        )
        .is_err());

        let output = program()
            .invoke("compute", OValue::str_("payload"))
            .unwrap();
        let OValue::Object { fields } = output else {
            panic!("wrap must return an object");
        };
        assert_eq!(fields.get("world"), Some(&OValue::str_("native.alpha")));
        assert_eq!(fields.get("input"), Some(&OValue::str_("payload")));
    }

    #[test]
    fn runtime_program_size_is_rejected_before_parse_or_file_read() {
        let oversized_text = "x".repeat(RUNTIME_MAX_PROGRAM_BYTES + 1);
        let parse_error = RuntimeProgram::parse(&oversized_text).unwrap_err();
        assert_eq!(
            parse_error.to_string(),
            format!(
                "runtime program exceeds {RUNTIME_MAX_PROGRAM_BYTES}-byte limit (got {})",
                RUNTIME_MAX_PROGRAM_BYTES + 1
            )
        );

        let directory = tempfile::tempdir().unwrap();
        let entry = directory.path().join("oversized-runtime.toml");
        fs::write(&entry, vec![0xff; RUNTIME_MAX_PROGRAM_BYTES + 1]).unwrap();
        let file = manifest::open_payload_regular_file(
            directory.path(),
            Path::new("oversized-runtime.toml"),
        )
        .unwrap();
        let read_error = read_runtime_program(file, &entry).unwrap_err();
        assert_eq!(read_error.to_string(), parse_error.to_string());
    }

    #[test]
    fn runtime_operation_count_has_an_exact_upper_bound() {
        let maximum = identity_program_with_operations(RUNTIME_MAX_OPERATIONS);
        assert_eq!(
            RuntimeProgram::parse(&maximum).unwrap().operations.len(),
            RUNTIME_MAX_OPERATIONS
        );

        let oversized = identity_program_with_operations(RUNTIME_MAX_OPERATIONS + 1);
        let error = RuntimeProgram::parse(&oversized).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "runtime program declares {} operations; maximum is {RUNTIME_MAX_OPERATIONS}",
                RUNTIME_MAX_OPERATIONS + 1
            )
        );
    }

    #[test]
    fn operation_controlled_messages_are_bounded() {
        let maximum = "m".repeat(RUNTIME_MAX_OPERATION_MESSAGE_BYTES);
        RuntimeProgram::parse(&program_with_message_operation("prefix", "text", &maximum)).unwrap();
        RuntimeProgram::parse(&program_with_message_operation("fail", "message", &maximum))
            .unwrap();

        let oversized = format!("{maximum}x");
        let prefix_error = RuntimeProgram::parse(&program_with_message_operation(
            "prefix", "text", &oversized,
        ))
        .unwrap_err();
        assert_eq!(
            prefix_error.to_string(),
            format!("prefix operation text exceeds {RUNTIME_MAX_OPERATION_MESSAGE_BYTES} bytes")
        );
        let fail_error = RuntimeProgram::parse(&program_with_message_operation(
            "fail", "message", &oversized,
        ))
        .unwrap_err();
        assert_eq!(
            fail_error.to_string(),
            format!("fail operation message exceeds {RUNTIME_MAX_OPERATION_MESSAGE_BYTES} bytes")
        );
    }

    #[test]
    fn worker_protocol_is_framed_and_service_scoped() {
        let mut requests = Vec::new();
        wire::write_frame(&mut requests, &RuntimeRequest::Health { nonce: 7 }).unwrap();
        wire::write_frame(
            &mut requests,
            &RuntimeRequest::Invoke {
                service: "world.alpha".into(),
                operation: "compute".into(),
                input: OValue::int(42),
            },
        )
        .unwrap();
        wire::write_frame(&mut requests, &RuntimeRequest::Shutdown).unwrap();

        let mut responses = Vec::new();
        serve(
            &program(),
            "world.alpha",
            &mut &requests[..],
            &mut responses,
        )
        .unwrap();

        let mut responses = &responses[..];
        assert!(matches!(
            wire::read_frame::<_, RuntimeResponse>(&mut responses)
                .unwrap()
                .unwrap(),
            RuntimeResponse::Healthy { nonce: 7, .. }
        ));
        assert!(matches!(
            wire::read_frame::<_, RuntimeResponse>(&mut responses)
                .unwrap()
                .unwrap(),
            RuntimeResponse::Result { .. }
        ));
        assert_eq!(
            wire::read_frame::<_, RuntimeResponse>(&mut responses)
                .unwrap()
                .unwrap(),
            RuntimeResponse::Stopped
        );
    }

    #[test]
    fn package_absolute_entry_resolves_only_beneath_payload_root() {
        let package = tempfile::tempdir().unwrap();
        fs::create_dir_all(package.path().join("bin")).unwrap();
        fs::write(package.path().join("bin/live"), b"runtime program").unwrap();

        let (_, resolved) = checked_entry(package.path(), "/bin/live").unwrap();
        assert_eq!(resolved, package.path().join("bin/live"));

        assert!(checked_entry(package.path(), "bin/live").is_err());
        assert!(checked_entry(package.path(), "/../bin/live").is_err());

        let outside = tempfile::NamedTempFile::new().unwrap();
        let host_absolute = outside.path().to_str().unwrap();
        assert!(Path::new(host_absolute).is_absolute());
        let package_relative = manifest::runtime_entry_payload_path(host_absolute).unwrap();
        let package_local = package.path().join(package_relative);
        fs::create_dir_all(package_local.parent().unwrap()).unwrap();
        fs::write(&package_local, b"package-local runtime").unwrap();

        let (_, resolved) = checked_entry(package.path(), host_absolute).unwrap();
        assert_eq!(resolved, package_local);
        assert_ne!(resolved, outside.path());
    }

    #[cfg(unix)]
    #[test]
    fn runtime_entry_rejects_symlinks_in_root_and_entry_components() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let real = temporary.path().join("real");
        fs::create_dir_all(real.join("payload/bin")).unwrap();
        fs::write(real.join("payload/bin/live"), b"runtime").unwrap();

        symlink(real.join("payload"), temporary.path().join("payload-link")).unwrap();
        assert!(checked_entry(&temporary.path().join("payload-link"), "/bin/live").is_err());

        symlink(real.join("payload/bin"), real.join("payload/bin-link")).unwrap();
        assert!(checked_entry(&real.join("payload"), "/bin-link/live").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn runtime_bytes_come_from_the_checked_descriptor() {
        let package = tempfile::tempdir().unwrap();
        fs::create_dir_all(package.path().join("bin")).unwrap();
        let path = package.path().join("bin/live");
        fs::write(&path, b"verified bytes").unwrap();

        let (opened, display_path) = checked_entry(package.path(), "/bin/live").unwrap();
        fs::rename(&path, package.path().join("bin/original")).unwrap();
        fs::write(&path, b"replacement bytes").unwrap();

        assert_eq!(
            read_runtime_program(opened, &display_path).unwrap(),
            "verified bytes"
        );
    }

    #[test]
    fn worker_rejects_oversized_frame_from_length_prefix() {
        let header = ((RUNTIME_MAX_FRAME_LEN + 1) as u32).to_be_bytes();
        let mut reader = header.as_slice();
        let mut responses = Vec::new();

        let error = serve(&program(), "world.alpha", &mut reader, &mut responses).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "wire frame length {} exceeds maximum {RUNTIME_MAX_FRAME_LEN}",
                RUNTIME_MAX_FRAME_LEN + 1
            )
        );
        assert!(reader.is_empty(), "only the length prefix was consumed");
        assert!(responses.is_empty());
    }

    #[test]
    fn worker_does_not_emit_oversized_response_frames() {
        let mut program = program();
        program.operations.insert(
            "compute".into(),
            OperationSpec::Prefix {
                text: "p".repeat(4096),
            },
        );
        let request = RuntimeRequest::Invoke {
            service: "world.alpha".into(),
            operation: "compute".into(),
            input: OValue::str_("x".repeat(RUNTIME_MAX_FRAME_LEN - 2048)),
        };
        assert!(wire::encode_message(&request).unwrap().len() <= RUNTIME_MAX_FRAME_LEN);
        let mut requests = Vec::new();
        wire::write_frame(&mut requests, &request).unwrap();
        let mut responses = Vec::new();

        let error = serve(&program, "world.alpha", &mut &requests[..], &mut responses).unwrap_err();

        assert!(error
            .to_string()
            .contains(&format!("exceeds maximum {RUNTIME_MAX_FRAME_LEN}")));
        assert!(responses.is_empty());
    }

    #[test]
    fn crash_operation_fails_worker_before_any_response() {
        let crash_source = r#"
schema = "ocore.runtime-program/v1"
world = "native.crash-test"

[health]
status = "healthy"

[operations.crash]
kind = "crash"
"#;
        let crash_program = RuntimeProgram::parse(crash_source).unwrap();
        assert!(RuntimeProgram::parse(&format!("{crash_source}unexpected = true\n")).is_err());
        let direct_error = crash_program
            .invoke("crash", OValue::Null)
            .unwrap_err()
            .to_string();
        assert!(direct_error.contains("only through the hosted worker protocol"));

        let mut request = Vec::new();
        wire::write_frame(
            &mut request,
            &RuntimeRequest::Invoke {
                service: "world.crash".into(),
                operation: "crash".into(),
                input: OValue::Null,
            },
        )
        .unwrap();
        let mut responses = Vec::new();

        let error = serve(
            &crash_program,
            "world.crash",
            &mut &request[..],
            &mut responses,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("hosted test worker crash requested"));
        assert!(responses.is_empty(), "a crashing worker must not reply");
    }
}
