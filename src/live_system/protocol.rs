//! Versioned process protocol for hosted native-live runtime packages.
//!
//! This is a Stage-1 reference surface. Runtime workers are ordinary child
//! processes and exchange only framed canonical-CBOR messages carrying
//! `OValue`s. They are not O-core processes and this module does not claim the
//! kernel isolation required by the native live-system milestone.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::value::OValue;
use crate::wire;

pub const RUNTIME_PROGRAM_SCHEMA: &str = "ocore.runtime-program/v1";
pub const RUNTIME_PROTOCOL: &str = "ocore.runtime-service/v1";

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
    Wrap { field: String },
    Prefix { text: String },
    IntPair { lhs: i64, rhs: i64 },
    SumFields { lhs: String, rhs: String },
    Fail { message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
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
pub(crate) enum RuntimeResponse {
    Healthy { nonce: u64, world: String },
    Unhealthy { nonce: u64, message: String },
    Result { value: OValue },
    Error { message: String },
    Stopped,
}

impl RuntimeProgram {
    pub fn parse(text: &str) -> Result<Self> {
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
        for (name, operation) in &self.operations {
            validate_identifier("operation", name)?;
            match operation {
                OperationSpec::Wrap { field } => validate_identifier("wrap field", field)?,
                OperationSpec::Prefix { text } if text.len() > 4096 => {
                    bail!("prefix operation text exceeds 4096 bytes")
                }
                OperationSpec::SumFields { lhs, rhs } => {
                    validate_identifier("sum lhs field", lhs)?;
                    validate_identifier("sum rhs field", rhs)?;
                    if lhs == rhs {
                        bail!("sum operation requires two distinct fields");
                    }
                }
                OperationSpec::Fail { message } if message.is_empty() => {
                    bail!("fail operation requires a non-empty message")
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

fn checked_entry(package_root: &Path, entry: &str) -> Result<std::path::PathBuf> {
    let relative = Path::new(entry);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        bail!("runtime entry must be a normalized relative path");
    }
    let root = package_root
        .canonicalize()
        .with_context(|| format!("failed to resolve package root {}", package_root.display()))?;
    let candidate = root.join(relative);
    let metadata = fs::symlink_metadata(&candidate)
        .with_context(|| format!("failed to inspect runtime entry {}", candidate.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("runtime entry must be a regular non-symlink file");
    }
    let candidate = candidate
        .canonicalize()
        .with_context(|| format!("failed to resolve runtime entry {}", candidate.display()))?;
    if !candidate.starts_with(&root) {
        bail!("runtime entry escapes its immutable package object");
    }
    Ok(candidate)
}

pub fn worker_main(package_root: &Path, entry: &str, service: &str) -> Result<()> {
    validate_identifier("service", service)?;
    let entry = checked_entry(package_root, entry)?;
    let text = fs::read_to_string(&entry)
        .with_context(|| format!("failed to read runtime entry {}", entry.display()))?;
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
    while let Some(request) = wire::read_frame::<_, RuntimeRequest>(reader)? {
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
                    match program.invoke(&operation, input) {
                        Ok(value) => RuntimeResponse::Result { value },
                        Err(error) => RuntimeResponse::Error {
                            message: error.to_string(),
                        },
                    }
                }
            }
            RuntimeRequest::Shutdown => {
                wire::write_frame(writer, &RuntimeResponse::Stopped)?;
                return Ok(());
            }
        };
        wire::write_frame(writer, &response)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
