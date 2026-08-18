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
pub const EVALUATOR_STATE_SNAPSHOT_SCHEMA_V1: &str = "ostadix.evaluator-state-snapshot/v1";
pub const EVALUATOR_ACTOR_CHECKPOINT_SCHEMA_V1: &str = "ostadix.evaluator-actor-checkpoint/v1";
pub const BACKEND_SANDBOX_POLICY_SCHEMA_V1: &str = "ostadix.backend-sandbox-policy/v1";
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

/// Canonical, authority-free metadata for one persistent evaluator actor.
///
/// This record is intentionally descriptive. It can prove that a future
/// admitted dispatch is the exact target for a pending restore, but it cannot
/// launch that target: live executable leases remain process-local authority.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorActorCheckpointV1 {
    pub schema: String,
    pub canonical_backend: String,
    pub environment_id: u32,
    pub sandbox_permissions: Vec<crate::value::BackendAuthority>,
    pub sandbox_policy_sha256: String,
    pub launch_generation_sha256: String,
    pub runtime_binding_sha256: String,
    pub checkpoint: BackendCheckpointV1,
}

impl EvaluatorActorCheckpointV1 {
    pub fn new(
        canonical_backend: impl Into<String>,
        environment_id: u32,
        sandbox_permissions: Vec<crate::value::BackendAuthority>,
        launch_generation_sha256: impl Into<String>,
        checkpoint: BackendCheckpointV1,
    ) -> Result<Self> {
        let runtime_binding_sha256 = checkpoint.runtime_binding_sha256.clone();
        let actor = Self {
            schema: EVALUATOR_ACTOR_CHECKPOINT_SCHEMA_V1.to_string(),
            canonical_backend: canonical_backend.into(),
            environment_id,
            sandbox_policy_sha256: sandbox_policy_sha256(&sandbox_permissions)?,
            sandbox_permissions,
            launch_generation_sha256: launch_generation_sha256.into(),
            runtime_binding_sha256,
            checkpoint,
        };
        actor.validate()?;
        Ok(actor)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != EVALUATOR_ACTOR_CHECKPOINT_SCHEMA_V1 {
            bail!(
                "unsupported evaluator actor-checkpoint schema `{}`",
                self.schema
            );
        }
        if self.canonical_backend.is_empty() {
            bail!("evaluator actor checkpoint requires a canonical backend");
        }
        if self.environment_id > crate::environment::MAX_PERSISTENT_ENV_ID {
            bail!(
                "evaluator actor checkpoint environment {} is not persistent",
                self.environment_id
            );
        }
        if self
            .sandbox_permissions
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            bail!("evaluator actor checkpoint permissions are not canonical");
        }
        validate_canonical_sha256("sandbox policy", &self.sandbox_policy_sha256)?;
        let actual_sandbox = sandbox_policy_sha256(&self.sandbox_permissions)?;
        if actual_sandbox != self.sandbox_policy_sha256 {
            bail!(
                "evaluator actor checkpoint sandbox digest mismatch: expected {}, got {actual_sandbox}",
                self.sandbox_policy_sha256
            );
        }
        validate_canonical_sha256("actor launch generation", &self.launch_generation_sha256)?;
        validate_canonical_sha256("actor runtime binding", &self.runtime_binding_sha256)?;
        self.checkpoint.validate()?;
        if self.checkpoint.backend != self.canonical_backend {
            bail!(
                "evaluator actor backend `{}` disagrees with checkpoint backend `{}`",
                self.canonical_backend,
                self.checkpoint.backend
            );
        }
        if self.checkpoint.runtime_binding_sha256 != self.runtime_binding_sha256 {
            bail!("evaluator actor runtime binding disagrees with its checkpoint");
        }
        if self.checkpoint.tier == BackendStateTierV1::ExternalPinned
            || !self.checkpoint.external_resources.is_empty()
        {
            bail!(
                "state.pin-required: external backend resources are not portable evaluator state"
            );
        }
        Ok(())
    }

    fn canonical_sort_key(&self) -> (&str, u32, &str, &str) {
        (
            &self.canonical_backend,
            self.environment_id,
            &self.sandbox_policy_sha256,
            &self.launch_generation_sha256,
        )
    }
}

/// Complete portable backend-owned state for an evaluator at settled actor
/// boundaries. Actor order is canonical so semantically identical snapshots
/// have identical wire bytes and digests.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorStateSnapshotV1 {
    pub schema: String,
    pub actors: Vec<EvaluatorActorCheckpointV1>,
}

impl EvaluatorStateSnapshotV1 {
    pub fn new(mut actors: Vec<EvaluatorActorCheckpointV1>) -> Result<Self> {
        actors.sort_by(|left, right| left.canonical_sort_key().cmp(&right.canonical_sort_key()));
        let snapshot = Self {
            schema: EVALUATOR_STATE_SNAPSHOT_SCHEMA_V1.to_string(),
            actors,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != EVALUATOR_STATE_SNAPSHOT_SCHEMA_V1 {
            bail!(
                "unsupported evaluator state-snapshot schema `{}`",
                self.schema
            );
        }
        for actor in &self.actors {
            actor.validate()?;
        }
        if self
            .actors
            .windows(2)
            .any(|pair| pair[0].canonical_sort_key() >= pair[1].canonical_sort_key())
        {
            bail!("evaluator actor checkpoints are not in unique canonical order");
        }
        if self.actors.windows(2).any(|pair| {
            pair[0].canonical_backend == pair[1].canonical_backend
                && pair[0].environment_id == pair[1].environment_id
                && pair[0].sandbox_policy_sha256 == pair[1].sandbox_policy_sha256
        }) {
            bail!("evaluator state snapshot contains a duplicate logical actor");
        }
        Ok(())
    }

    pub fn encoded_len(&self) -> Result<usize> {
        Ok(self.canonical_bytes()?.len())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        crate::wire::encode_message(self)
    }

    pub fn snapshot_sha256(&self) -> Result<String> {
        Ok(hex::encode(Sha256::digest(self.canonical_bytes()?)))
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

pub fn ensure_evaluator_snapshot_bound(
    snapshot: &EvaluatorStateSnapshotV1,
    max_bytes: u64,
) -> Result<()> {
    if max_bytes == 0 {
        bail!("evaluator snapshot byte limit must be non-zero");
    }
    snapshot.validate()?;
    let encoded = snapshot.encoded_len()?;
    let limit: usize = max_bytes
        .try_into()
        .context("evaluator snapshot byte limit exceeds host address space")?;
    if encoded > limit {
        bail!("evaluator snapshot length {encoded} exceeds requested maximum {limit}");
    }
    Ok(())
}

pub fn sandbox_policy_sha256(permissions: &[crate::value::BackendAuthority]) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(BACKEND_SANDBOX_POLICY_SCHEMA_V1.as_bytes());
    digest.update([0]);
    digest.update(crate::wire::encode_message(&permissions.to_vec())?);
    Ok(hex::encode(digest.finalize()))
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

fn validate_canonical_sha256(field: &str, value: &str) -> Result<()> {
    validate_sha256(field, value)?;
    if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        bail!("{field} is not canonical lowercase SHA-256");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{BackendAuthority, OWireCommand, OWireResponse};

    fn evaluator_actor(environment_id: u32, launch_byte: &str) -> EvaluatorActorCheckpointV1 {
        EvaluatorActorCheckpointV1::new(
            "bash",
            environment_id,
            vec![BackendAuthority::FileRead, BackendAuthority::Process],
            launch_byte.repeat(32),
            empty_checkpoint("bash", &"11".repeat(32)).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn evaluator_snapshot_bytes_are_canonical_bounded_and_round_trip() {
        let snapshot =
            EvaluatorStateSnapshotV1::new(vec![evaluator_actor(9, "22"), evaluator_actor(3, "33")])
                .unwrap();
        assert_eq!(snapshot.actors[0].environment_id, 3);
        assert_eq!(snapshot.actors[1].environment_id, 9);

        let bytes = snapshot.canonical_bytes().unwrap();
        let decoded: EvaluatorStateSnapshotV1 = crate::wire::decode_message(&bytes).unwrap();
        assert_eq!(decoded, snapshot);
        ensure_evaluator_snapshot_bound(&snapshot, bytes.len() as u64).unwrap();
        assert!(ensure_evaluator_snapshot_bound(&snapshot, bytes.len() as u64 - 1).is_err());
        assert_eq!(snapshot.snapshot_sha256().unwrap().len(), 64);
    }

    #[test]
    fn evaluator_snapshot_rejects_duplicate_logical_actor() {
        let first = evaluator_actor(7, "22");
        let mut second = first.clone();
        second.launch_generation_sha256 = "33".repeat(32);
        let error = EvaluatorStateSnapshotV1::new(vec![first, second]).unwrap_err();
        assert!(error.to_string().contains("duplicate logical actor"));
    }

    #[test]
    fn evaluator_actor_checkpoint_rejects_external_resources_as_portable() {
        let checkpoint = BackendCheckpointV1::new(
            "ubuntu_vm",
            BackendStateTierV1::ExternalPinned,
            "ostadix.external-vm/v1",
            "11".repeat(32),
            serde_json::json!({ "kind": "external" }),
            vec![BackendExternalResourceV1 {
                kind: "virtual-machine".to_string(),
                identity: "vm-7".to_string(),
                recovery: "continue-pinned".to_string(),
                metadata: BTreeMap::new(),
            }],
        )
        .unwrap();
        let error = EvaluatorActorCheckpointV1::new(
            "ubuntu_vm",
            7,
            Vec::new(),
            "22".repeat(32),
            checkpoint,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not portable evaluator state"));
    }

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
