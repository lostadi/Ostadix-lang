//! Versioned backend-owned state protocol.
//!
//! This module deliberately does not replace the public `OWireCommand` /
//! `OWireResponse` vocabulary in `value.rs`.  The internal V2 envelope preserves
//! those legacy serde shapes byte-for-byte and adds explicit state messages.
//! Backend checkpoints cover only state owned by the backend actor.  Files,
//! services, child processes, Nix store objects, and other external resources
//! require separate bindings and are never implied by an empty checkpoint.

use std::collections::{BTreeMap, HashMap};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::value::OValue;

pub const BACKEND_STATE_PROTOCOL_V1: &str = "ostadix.backend-state/v1";
pub const BACKEND_STATE_CAPABILITIES_SCHEMA_V1: &str = "ostadix.backend-state-capabilities/v1";
pub const BACKEND_CHECKPOINT_SCHEMA_V1: &str = "ostadix.backend-checkpoint/v1";
pub const BACKEND_RESTORE_RECEIPT_SCHEMA_V1: &str = "ostadix.backend-restore-receipt/v1";
pub const BACKEND_STATE_REASON_SCHEMA_V1: &str = "ostadix.backend-state-reason/v1";
pub const BACKEND_STATE_ERROR_SCHEMA_V1: &str = "ostadix.backend-state-error/v1";
pub const STATELESS_EMPTY_CODEC_V1: &str = "ostadix.backend-empty/v1";
pub const SQL_CLI_CODEC_V1: &str = "ostadix.sqlite-cli-main/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendStateTierV1 {
    Stateless,
    SemanticSnapshot,
    ExternalPinned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendStateCapabilitiesV1 {
    pub schema: String,
    pub protocol: String,
    pub backend: String,
    pub tier: BackendStateTierV1,
    pub codec: String,
    /// A state codec covers backend-owned state at a settled command boundary.
    /// It never claims to capture ambient files, services, or escaped processes.
    pub scope: String,
    pub restore_supported: bool,
}

impl BackendStateCapabilitiesV1 {
    pub fn new(
        backend: impl Into<String>,
        tier: BackendStateTierV1,
        codec: impl Into<String>,
        restore_supported: bool,
    ) -> Self {
        Self {
            schema: BACKEND_STATE_CAPABILITIES_SCHEMA_V1.to_string(),
            protocol: BACKEND_STATE_PROTOCOL_V1.to_string(),
            backend: backend.into(),
            tier,
            codec: codec.into(),
            scope: "backend-owned-state-at-settled-command-boundary".to_string(),
            restore_supported,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != BACKEND_STATE_CAPABILITIES_SCHEMA_V1 {
            bail!(
                "unsupported backend state-capabilities schema `{}`",
                self.schema
            );
        }
        if self.protocol != BACKEND_STATE_PROTOCOL_V1 {
            bail!("unsupported backend state protocol `{}`", self.protocol);
        }
        if self.backend.is_empty() || self.codec.is_empty() {
            bail!("backend state capabilities require non-empty backend and codec identities");
        }
        if self.scope != "backend-owned-state-at-settled-command-boundary" {
            bail!(
                "backend state capabilities advertise unsupported scope `{}`",
                self.scope
            );
        }
        if self.tier == BackendStateTierV1::ExternalPinned && self.restore_supported {
            bail!("external-pinned backend state cannot advertise portable restore support");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendExternalResourceV1 {
    pub kind: String,
    pub identity: String,
    pub recovery: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendCheckpointV1 {
    pub schema: String,
    pub protocol: String,
    pub backend: String,
    pub tier: BackendStateTierV1,
    pub codec: String,
    pub runtime_binding_sha256: String,
    pub payload: Value,
    pub payload_sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_resources: Vec<BackendExternalResourceV1>,
}

impl BackendCheckpointV1 {
    pub fn new(
        backend: impl Into<String>,
        tier: BackendStateTierV1,
        codec: impl Into<String>,
        runtime_binding_sha256: impl Into<String>,
        payload: Value,
        external_resources: Vec<BackendExternalResourceV1>,
    ) -> Result<Self> {
        let checkpoint = Self {
            schema: BACKEND_CHECKPOINT_SCHEMA_V1.to_string(),
            protocol: BACKEND_STATE_PROTOCOL_V1.to_string(),
            backend: backend.into(),
            tier,
            codec: codec.into(),
            runtime_binding_sha256: runtime_binding_sha256.into(),
            payload_sha256: payload_sha256(&payload)?,
            payload,
            external_resources,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != BACKEND_CHECKPOINT_SCHEMA_V1 {
            bail!("unsupported backend checkpoint schema `{}`", self.schema);
        }
        if self.protocol != BACKEND_STATE_PROTOCOL_V1 {
            bail!("unsupported backend state protocol `{}`", self.protocol);
        }
        if self.backend.is_empty() || self.codec.is_empty() {
            bail!("backend checkpoint requires non-empty backend and codec identities");
        }
        validate_sha256("checkpoint runtime binding", &self.runtime_binding_sha256)?;
        validate_sha256("checkpoint payload", &self.payload_sha256)?;
        let actual = payload_sha256(&self.payload)?;
        if actual != self.payload_sha256 {
            bail!(
                "backend checkpoint payload digest mismatch: expected {}, got {actual}",
                self.payload_sha256
            );
        }
        for resource in &self.external_resources {
            if resource.kind.is_empty()
                || resource.identity.is_empty()
                || resource.recovery.is_empty()
            {
                bail!("backend checkpoint contains an incomplete external-resource binding");
            }
        }
        match self.tier {
            BackendStateTierV1::ExternalPinned if self.external_resources.is_empty() => {
                bail!("external-pinned checkpoint omitted its resource binding")
            }
            BackendStateTierV1::Stateless | BackendStateTierV1::SemanticSnapshot
                if !self.external_resources.is_empty() =>
            {
                bail!("portable backend checkpoint contains external resource bindings")
            }
            _ => {}
        }
        Ok(())
    }

    pub fn checkpoint_sha256(&self) -> Result<String> {
        Ok(hex::encode(Sha256::digest(crate::wire::encode_message(
            self,
        )?)))
    }

    pub fn encoded_len(&self) -> Result<usize> {
        Ok(crate::wire::encode_message(self)?.len())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendRestoreReceiptV1 {
    pub schema: String,
    pub protocol: String,
    pub backend: String,
    pub checkpoint_sha256: String,
    pub restored: bool,
}

impl BackendRestoreReceiptV1 {
    pub fn restored(backend: impl Into<String>, checkpoint: &BackendCheckpointV1) -> Result<Self> {
        Ok(Self {
            schema: BACKEND_RESTORE_RECEIPT_SCHEMA_V1.to_string(),
            protocol: BACKEND_STATE_PROTOCOL_V1.to_string(),
            backend: backend.into(),
            checkpoint_sha256: checkpoint.checkpoint_sha256()?,
            restored: true,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendStateReasonV1 {
    pub schema: String,
    pub backend: String,
    pub code: String,
    pub path: String,
    pub message: String,
    pub recovery: String,
}

impl BackendStateReasonV1 {
    pub fn pin_required(
        backend: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            schema: BACKEND_STATE_REASON_SCHEMA_V1.to_string(),
            backend: backend.into(),
            code: "state.pin-required".to_string(),
            path: path.into(),
            message: message.into(),
            recovery: "continue-pinned".to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendStateErrorV1 {
    pub schema: String,
    pub backend: String,
    pub code: String,
    pub message: String,
}

impl BackendStateErrorV1 {
    pub fn new(
        backend: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            schema: BACKEND_STATE_ERROR_SCHEMA_V1.to_string(),
            backend: backend.into(),
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Internal backend wire V2. Legacy variants intentionally match the serde
/// shape of `value::OWireCommand`; tests lock that compatibility down.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum BackendWireCommandV2 {
    Exec {
        code: String,
        bindings: HashMap<String, OValue>,
    },
    Cleanup,
    Shutdown,
    Ping,
    #[serde(rename = "eval_result")]
    EvalResult {
        value: OValue,
    },
    #[serde(rename = "state_capabilities_v1")]
    StateCapabilitiesV1,
    #[serde(rename = "checkpoint_v1")]
    CheckpointV1 {
        max_bytes: u64,
    },
    #[serde(rename = "restore_v1")]
    RestoreV1 {
        checkpoint: BackendCheckpointV1,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum BackendWireResponseV2 {
    Ok {
        value: OValue,
    },
    Err {
        message: String,
    },
    #[serde(rename = "eval_request")]
    EvalRequest {
        src: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<OValue>,
    },
    #[serde(rename = "state_capabilities_v1")]
    StateCapabilitiesV1 {
        capabilities: BackendStateCapabilitiesV1,
    },
    #[serde(rename = "checkpoint_v1")]
    CheckpointV1 {
        checkpoint: BackendCheckpointV1,
    },
    #[serde(rename = "restore_v1")]
    RestoreV1 {
        receipt: BackendRestoreReceiptV1,
    },
    #[serde(rename = "state_pin_required_v1")]
    StatePinRequiredV1 {
        reason: BackendStateReasonV1,
    },
    #[serde(rename = "state_error_v1")]
    StateErrorV1 {
        error: BackendStateErrorV1,
    },
}

impl BackendWireResponseV2 {
    pub fn ok(value: OValue) -> Self {
        Self::Ok { value }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self::Err {
            message: message.into(),
        }
    }
}

pub fn empty_state_capabilities(backend: &str) -> BackendStateCapabilitiesV1 {
    BackendStateCapabilitiesV1::new(
        backend,
        BackendStateTierV1::Stateless,
        STATELESS_EMPTY_CODEC_V1,
        true,
    )
}

pub fn empty_checkpoint(
    backend: &str,
    runtime_binding_sha256: &str,
) -> Result<BackendCheckpointV1> {
    BackendCheckpointV1::new(
        backend,
        BackendStateTierV1::Stateless,
        STATELESS_EMPTY_CODEC_V1,
        runtime_binding_sha256,
        serde_json::json!({ "kind": "empty" }),
        Vec::new(),
    )
}

pub fn validate_empty_restore(
    backend: &str,
    runtime_binding_sha256: &str,
    checkpoint: &BackendCheckpointV1,
) -> Result<()> {
    checkpoint.validate()?;
    if checkpoint.backend != backend
        || checkpoint.tier != BackendStateTierV1::Stateless
        || checkpoint.codec != STATELESS_EMPTY_CODEC_V1
        || checkpoint.runtime_binding_sha256 != runtime_binding_sha256
        || checkpoint.payload != serde_json::json!({ "kind": "empty" })
        || !checkpoint.external_resources.is_empty()
    {
        bail!("stateless checkpoint is incompatible with backend `{backend}`");
    }
    Ok(())
}

pub fn ensure_checkpoint_bound(checkpoint: &BackendCheckpointV1, max_bytes: u64) -> Result<()> {
    if max_bytes == 0 {
        bail!("checkpoint byte limit must be non-zero");
    }
    let encoded = checkpoint.encoded_len()?;
    let limit: usize = max_bytes
        .try_into()
        .context("checkpoint byte limit exceeds host address space")?;
    if encoded > limit {
        bail!("checkpoint length {encoded} exceeds requested maximum {limit}");
    }
    Ok(())
}

pub fn payload_sha256(payload: &Value) -> Result<String> {
    Ok(hex::encode(Sha256::digest(crate::wire::encode_message(
        payload,
    )?)))
}

fn validate_sha256(field: &str, value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{field} is not a 64-character SHA-256 digest");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{OWireCommand, OWireResponse};

    #[test]
    fn legacy_command_shapes_are_byte_identical() {
        let bindings = HashMap::from([("value".to_string(), OValue::int(42))]);
        let pairs = [
            (
                OWireCommand::Exec {
                    code: "value + 1".to_string(),
                    bindings: bindings.clone(),
                },
                BackendWireCommandV2::Exec {
                    code: "value + 1".to_string(),
                    bindings,
                },
            ),
            (OWireCommand::Ping, BackendWireCommandV2::Ping),
            (OWireCommand::Cleanup, BackendWireCommandV2::Cleanup),
            (OWireCommand::Shutdown, BackendWireCommandV2::Shutdown),
            (
                OWireCommand::EvalResult {
                    value: OValue::Null,
                },
                BackendWireCommandV2::EvalResult {
                    value: OValue::Null,
                },
            ),
        ];
        for (legacy, state_aware) in pairs {
            assert_eq!(
                crate::wire::encode_message(&legacy).unwrap(),
                crate::wire::encode_message(&state_aware).unwrap()
            );
        }
    }

    #[test]
    fn legacy_response_shapes_are_byte_identical() {
        let pairs = [
            (
                OWireResponse::ok(OValue::Null),
                BackendWireResponseV2::ok(OValue::Null),
            ),
            (
                OWireResponse::err("boom"),
                BackendWireResponseV2::err("boom"),
            ),
            (
                OWireResponse::EvalRequest {
                    src: "1 + 2".to_string(),
                    scope: Some(OValue::map(HashMap::from([(
                        "x".to_string(),
                        OValue::int(1),
                    )]))),
                },
                BackendWireResponseV2::EvalRequest {
                    src: "1 + 2".to_string(),
                    scope: Some(OValue::map(HashMap::from([(
                        "x".to_string(),
                        OValue::int(1),
                    )]))),
                },
            ),
        ];
        for (legacy, state_aware) in pairs {
            assert_eq!(
                crate::wire::encode_message(&legacy).unwrap(),
                crate::wire::encode_message(&state_aware).unwrap()
            );
        }
    }
}
