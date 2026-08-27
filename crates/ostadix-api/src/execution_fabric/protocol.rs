use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::world::{PortableOValue, PortableValueRecord};

pub type Sha256DigestV1 = [u8; 32];

pub const EXECUTION_CAPSULE_SCHEMA_V1: &str = "ostadix.oir-execution-capsule/v1";
pub const EXECUTION_CANDIDATE_SCHEMA_V1: &str = "ostadix.oir-execution-candidate/v1";
const SOURCE_CLOSED_RENDERER_SCHEMA_V1: &str = "ostadix.source-closed-renderer/v1";
const INPUT_MANIFEST_SCHEMA_V1: &str = "ostadix.execution-input-manifest/v1";
const OUTPUT_CONTRACT_SCHEMA_V1: &str = "ostadix.execution-output-contract/v1";
const TRUSTED_INLINE_RENDERER_ADAPTER_V1: &str = "trusted-inline-renderer/v1";
const WIRE_VERSION_V1: u16 = 1;

pub const MAX_EXECUTION_CAPSULE_BYTES: usize = 64 * 1024;
pub const MAX_EXECUTION_CANDIDATE_BYTES: usize = 16 * 1024;
const MAX_INPUT_BINDINGS: usize = 8;
const MAX_RENDERER_PARTS: usize = 128;
const MAX_RENDERER_LITERAL_BYTES: usize = 4 * 1024;
const MAX_RENDERER_SOURCE_BYTES: usize = 16 * 1024;
const MAX_SLOT_BYTES: usize = 64;
const MAX_RUNTIME_MS: u64 = 300_000;
const MAX_INPUT_VALUE_BYTES_TOTAL: usize = 32 * 1024;
const MAX_OUTPUT_BYTES: u32 = crate::world::MAX_OVALUE_RECORD_BYTES;
const MAX_FAILURE_MESSAGE_BYTES: usize = 1024;

#[derive(Debug, Error)]
pub enum ExecutionFabricError {
    #[error("invalid execution-fabric record: {0}")]
    Invalid(String),
    #[error("execution-fabric {kind} record is {actual} bytes; maximum is {maximum}")]
    RecordTooLarge {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("execution-fabric {kind} record is not canonical CBOR")]
    NonCanonical { kind: &'static str },
    #[error("execution-fabric codec error: {0}")]
    Codec(String),
    #[error("execution-fabric portable-value error: {0}")]
    PortableValue(String),
}

/// Executor disposition required for a fabric-layer rejection.
///
/// This is deliberately not a wire field. A candidate-reported semantic
/// failure remains inert until a future executor-side adapter proves that the
/// capsule admitted a fallible operation. Decode, canonicality, binding,
/// authority, deadline, and output-contract failures are never O-language
/// node failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionFabricFailureClassV1 {
    InfrastructureAbort,
}

impl ExecutionFabricError {
    pub fn failure_class(&self) -> ExecutionFabricFailureClassV1 {
        ExecutionFabricFailureClassV1::InfrastructureAbort
    }
}

fn invalid(message: impl Into<String>) -> ExecutionFabricError {
    ExecutionFabricError::Invalid(message.into())
}

fn require_nonzero_digest(label: &str, value: &Sha256DigestV1) -> Result<(), ExecutionFabricError> {
    if value.iter().all(|byte| *byte == 0) {
        return Err(invalid(format!("{label} must not be the all-zero digest")));
    }
    Ok(())
}

pub(crate) fn domain_sha256(domain: &[u8], payload: &[u8]) -> Sha256DigestV1 {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update([0]);
    digest.update(payload);
    digest.finalize().into()
}

fn canonical_digest<T: Serialize>(
    domain: &[u8],
    value: &T,
) -> Result<Sha256DigestV1, ExecutionFabricError> {
    let encoded = crate::canonical_cbor::encode(value)
        .map_err(|error| ExecutionFabricError::Codec(format!("{error:#}")))?;
    Ok(domain_sha256(domain, &encoded))
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ExecutionIdV1(Sha256DigestV1);

impl ExecutionIdV1 {
    pub fn new(value: Sha256DigestV1) -> Result<Self, ExecutionFabricError> {
        require_nonzero_digest("execution identity", &value)?;
        Ok(Self(value))
    }

    pub fn as_bytes(&self) -> &Sha256DigestV1 {
        &self.0
    }

    fn validate(&self) -> Result<(), ExecutionFabricError> {
        require_nonzero_digest("execution identity", &self.0)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct LogicalTaskIdV1 {
    pub(crate) execution: ExecutionIdV1,
    pub(crate) semantic_sha256: Sha256DigestV1,
}

impl LogicalTaskIdV1 {
    pub fn new(
        execution: ExecutionIdV1,
        semantic_sha256: Sha256DigestV1,
    ) -> Result<Self, ExecutionFabricError> {
        let value = Self {
            execution,
            semantic_sha256,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn execution(&self) -> &ExecutionIdV1 {
        &self.execution
    }

    pub fn semantic_sha256(&self) -> &Sha256DigestV1 {
        &self.semantic_sha256
    }

    fn validate(&self) -> Result<(), ExecutionFabricError> {
        self.execution.validate()?;
        require_nonzero_digest("logical task semantic identity", &self.semantic_sha256)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct AttemptIdV1 {
    pub(crate) task: LogicalTaskIdV1,
    pub(crate) generation: u64,
}

impl AttemptIdV1 {
    pub fn new(task: LogicalTaskIdV1, generation: u64) -> Result<Self, ExecutionFabricError> {
        let value = Self { task, generation };
        value.validate()?;
        Ok(value)
    }

    pub fn task(&self) -> &LogicalTaskIdV1 {
        &self.task
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn validate(&self) -> Result<(), ExecutionFabricError> {
        self.task.validate()?;
        if self.generation == 0 {
            return Err(invalid("attempt generation must be nonzero"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustedInlineRendererV1 {
    Html,
    Markdown,
    Latex,
    Text,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RendererPartV1 {
    Literal { utf8: String },
    Input { slot: String },
}

impl RendererPartV1 {
    pub fn literal(utf8: impl Into<String>) -> Self {
        Self::Literal { utf8: utf8.into() }
    }

    pub fn input(slot: impl Into<String>) -> Self {
        Self::Input { slot: slot.into() }
    }
}

#[derive(Serialize)]
struct RendererSourceMaterial<'a> {
    renderer: TrustedInlineRendererV1,
    parts: &'a [RendererPartV1],
}

#[derive(Serialize)]
struct RendererRegionMaterial<'a> {
    adapter: &'a str,
    source_sha256: Sha256DigestV1,
    expected_oir_sha256: Sha256DigestV1,
    expected_plan_sha256: Sha256DigestV1,
    backend_catalog_sha256: Sha256DigestV1,
    backend_implementation_sha256: Sha256DigestV1,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SourceClosedRendererV1 {
    pub(crate) schema: String,
    pub(crate) version: u16,
    pub(crate) adapter: String,
    pub(crate) renderer: TrustedInlineRendererV1,
    pub(crate) parts: Vec<RendererPartV1>,
    pub(crate) source_sha256: Sha256DigestV1,
    pub(crate) expected_oir_sha256: Sha256DigestV1,
    pub(crate) expected_plan_sha256: Sha256DigestV1,
    pub(crate) backend_catalog_sha256: Sha256DigestV1,
    pub(crate) backend_implementation_sha256: Sha256DigestV1,
    pub(crate) region_sha256: Sha256DigestV1,
}

impl SourceClosedRendererV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        renderer: TrustedInlineRendererV1,
        parts: Vec<RendererPartV1>,
        expected_oir_sha256: Sha256DigestV1,
        expected_plan_sha256: Sha256DigestV1,
        backend_catalog_sha256: Sha256DigestV1,
        backend_implementation_sha256: Sha256DigestV1,
    ) -> Result<Self, ExecutionFabricError> {
        let source_sha256 = canonical_digest(
            b"ostadix/execution-fabric/renderer-source/v1",
            &RendererSourceMaterial {
                renderer,
                parts: &parts,
            },
        )?;
        let region_sha256 = canonical_digest(
            b"ostadix/execution-fabric/renderer-region/v1",
            &RendererRegionMaterial {
                adapter: TRUSTED_INLINE_RENDERER_ADAPTER_V1,
                source_sha256,
                expected_oir_sha256,
                expected_plan_sha256,
                backend_catalog_sha256,
                backend_implementation_sha256,
            },
        )?;
        let value = Self {
            schema: SOURCE_CLOSED_RENDERER_SCHEMA_V1.to_string(),
            version: WIRE_VERSION_V1,
            adapter: TRUSTED_INLINE_RENDERER_ADAPTER_V1.to_string(),
            renderer,
            parts,
            source_sha256,
            expected_oir_sha256,
            expected_plan_sha256,
            backend_catalog_sha256,
            backend_implementation_sha256,
            region_sha256,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn renderer(&self) -> TrustedInlineRendererV1 {
        self.renderer
    }

    pub fn parts(&self) -> &[RendererPartV1] {
        &self.parts
    }

    pub fn region_sha256(&self) -> &Sha256DigestV1 {
        &self.region_sha256
    }

    pub(crate) fn validate(&self) -> Result<(), ExecutionFabricError> {
        if self.schema != SOURCE_CLOSED_RENDERER_SCHEMA_V1 || self.version != WIRE_VERSION_V1 {
            return Err(invalid("unsupported source-closed renderer schema/version"));
        }
        if self.adapter != TRUSTED_INLINE_RENDERER_ADAPTER_V1 {
            return Err(invalid(
                "source-closed renderer adapter is not trusted-inline-renderer/v1",
            ));
        }
        if self.parts.len() > MAX_RENDERER_PARTS {
            return Err(invalid(format!(
                "renderer has {} parts; maximum is {MAX_RENDERER_PARTS}",
                self.parts.len()
            )));
        }
        let mut source_bytes = 0usize;
        for part in &self.parts {
            match part {
                RendererPartV1::Literal { utf8 } => {
                    if utf8.len() > MAX_RENDERER_LITERAL_BYTES {
                        return Err(invalid("renderer literal exceeds its byte limit"));
                    }
                    source_bytes = source_bytes
                        .checked_add(utf8.len())
                        .ok_or_else(|| invalid("renderer source byte count overflowed"))?;
                }
                RendererPartV1::Input { slot } => validate_slot(slot)?,
            }
        }
        if source_bytes > MAX_RENDERER_SOURCE_BYTES {
            return Err(invalid(
                "renderer literal source exceeds its aggregate byte limit",
            ));
        }
        for (label, digest) in [
            ("expected OIR", &self.expected_oir_sha256),
            ("expected plan", &self.expected_plan_sha256),
            ("backend catalog", &self.backend_catalog_sha256),
            (
                "backend implementation",
                &self.backend_implementation_sha256,
            ),
        ] {
            require_nonzero_digest(label, digest)?;
        }
        let expected_source = canonical_digest(
            b"ostadix/execution-fabric/renderer-source/v1",
            &RendererSourceMaterial {
                renderer: self.renderer,
                parts: &self.parts,
            },
        )?;
        if self.source_sha256 != expected_source {
            return Err(invalid("renderer source digest mismatch"));
        }
        let expected_region = canonical_digest(
            b"ostadix/execution-fabric/renderer-region/v1",
            &RendererRegionMaterial {
                adapter: &self.adapter,
                source_sha256: self.source_sha256,
                expected_oir_sha256: self.expected_oir_sha256,
                expected_plan_sha256: self.expected_plan_sha256,
                backend_catalog_sha256: self.backend_catalog_sha256,
                backend_implementation_sha256: self.backend_implementation_sha256,
            },
        )?;
        if self.region_sha256 != expected_region {
            return Err(invalid("renderer region digest mismatch"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PortableValueV1 {
    pub(crate) owvalue: Vec<u8>,
    pub(crate) content_sha256: Sha256DigestV1,
}

impl PortableValueV1 {
    pub fn new(record: &PortableValueRecord) -> Result<Self, ExecutionFabricError> {
        let PortableValueRecord::Core(_) = record else {
            return Err(invalid(
                "portable extensions are not admitted in execution capsules",
            ));
        };
        let owvalue = record
            .encode()
            .map_err(|error| ExecutionFabricError::PortableValue(error.to_string()))?;
        let decoded = PortableValueRecord::decode(&owvalue)
            .map_err(|error| ExecutionFabricError::PortableValue(error.to_string()))?;
        let canonical = decoded
            .encode()
            .map_err(|error| ExecutionFabricError::PortableValue(error.to_string()))?;
        if canonical != owvalue {
            return Err(invalid("OWVALUE input is not canonical"));
        }
        let PortableValueRecord::Core(value) = &decoded else {
            return Err(invalid(
                "portable extensions are not admitted in execution capsules",
            ));
        };
        validate_renderer_value(value)?;
        let content_sha256 = domain_sha256(b"ostadix/execution-fabric/portable-value/v1", &owvalue);
        Ok(Self {
            owvalue,
            content_sha256,
        })
    }

    pub fn decode(&self) -> Result<PortableValueRecord, ExecutionFabricError> {
        self.validate()?;
        PortableValueRecord::decode(&self.owvalue)
            .map_err(|error| ExecutionFabricError::PortableValue(error.to_string()))
    }

    pub fn encoded(&self) -> &[u8] {
        &self.owvalue
    }

    pub fn content_sha256(&self) -> &Sha256DigestV1 {
        &self.content_sha256
    }

    fn validate(&self) -> Result<(), ExecutionFabricError> {
        let record = PortableValueRecord::decode(&self.owvalue)
            .map_err(|error| ExecutionFabricError::PortableValue(error.to_string()))?;
        let canonical = record
            .encode()
            .map_err(|error| ExecutionFabricError::PortableValue(error.to_string()))?;
        if canonical != self.owvalue {
            return Err(invalid("embedded OWVALUE is not canonical"));
        }
        let PortableValueRecord::Core(value) = &record else {
            return Err(invalid(
                "portable extensions are not admitted in execution capsules",
            ));
        };
        validate_renderer_value(value)?;
        let expected = domain_sha256(b"ostadix/execution-fabric/portable-value/v1", &self.owvalue);
        if self.content_sha256 != expected {
            return Err(invalid("portable value content digest mismatch"));
        }
        Ok(())
    }
}

fn validate_renderer_value(value: &PortableOValue) -> Result<(), ExecutionFabricError> {
    match value {
        PortableOValue::Null
        | PortableOValue::Bool(_)
        | PortableOValue::Number(_)
        | PortableOValue::Text(_)
        | PortableOValue::Char(_) => Ok(()),
        PortableOValue::List(items) => {
            for item in items {
                validate_renderer_value(item)?;
            }
            Ok(())
        }
        PortableOValue::Record(fields) => {
            for (_, value) in fields {
                validate_renderer_value(value)?;
            }
            Ok(())
        }
        PortableOValue::Map(entries) => {
            for (key, value) in entries {
                validate_renderer_value(key)?;
                validate_renderer_value(value)?;
            }
            Ok(())
        }
        PortableOValue::Bytes(_)
        | PortableOValue::Tagged(_)
        | PortableOValue::CodeRef(_)
        | PortableOValue::ObjectRef(_)
        | PortableOValue::Error(_) => Err(invalid(
            "portable value kind is outside the trusted renderer allowlist",
        )),
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct InputBindingV1 {
    pub(crate) slot: String,
    pub(crate) value: PortableValueV1,
}

impl InputBindingV1 {
    pub fn new(
        slot: impl Into<String>,
        value: &PortableValueRecord,
    ) -> Result<Self, ExecutionFabricError> {
        let value = Self {
            slot: slot.into(),
            value: PortableValueV1::new(value)?,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn slot(&self) -> &str {
        &self.slot
    }

    pub fn value(&self) -> &PortableValueV1 {
        &self.value
    }

    fn validate(&self) -> Result<(), ExecutionFabricError> {
        validate_slot(&self.slot)?;
        self.value.validate()
    }
}

#[derive(Serialize)]
struct InputManifestMaterial<'a> {
    bindings: &'a [InputBindingV1],
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct InputManifestV1 {
    pub(crate) schema: String,
    pub(crate) version: u16,
    pub(crate) bindings: Vec<InputBindingV1>,
    pub(crate) manifest_sha256: Sha256DigestV1,
}

impl InputManifestV1 {
    pub fn new(mut bindings: Vec<InputBindingV1>) -> Result<Self, ExecutionFabricError> {
        bindings.sort_by(|left, right| left.slot.as_bytes().cmp(right.slot.as_bytes()));
        let manifest_sha256 = canonical_digest(
            b"ostadix/execution-fabric/input-manifest/v1",
            &InputManifestMaterial {
                bindings: &bindings,
            },
        )?;
        let value = Self {
            schema: INPUT_MANIFEST_SCHEMA_V1.to_string(),
            version: WIRE_VERSION_V1,
            bindings,
            manifest_sha256,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn bindings(&self) -> &[InputBindingV1] {
        &self.bindings
    }

    pub fn manifest_sha256(&self) -> &Sha256DigestV1 {
        &self.manifest_sha256
    }

    pub fn binding(&self, slot: &str) -> Option<&InputBindingV1> {
        self.bindings
            .binary_search_by(|binding| binding.slot.as_str().cmp(slot))
            .ok()
            .map(|index| &self.bindings[index])
    }

    pub(crate) fn validate(&self) -> Result<(), ExecutionFabricError> {
        if self.schema != INPUT_MANIFEST_SCHEMA_V1 || self.version != WIRE_VERSION_V1 {
            return Err(invalid("unsupported input manifest schema/version"));
        }
        if self.bindings.len() > MAX_INPUT_BINDINGS {
            return Err(invalid("input manifest has too many bindings"));
        }
        let mut prior: Option<&str> = None;
        let mut total = 0usize;
        for binding in &self.bindings {
            binding.validate()?;
            if prior.is_some_and(|slot| slot.as_bytes() >= binding.slot.as_bytes()) {
                return Err(invalid("input bindings must be uniquely sorted by slot"));
            }
            prior = Some(&binding.slot);
            total = total
                .checked_add(binding.value.owvalue.len())
                .ok_or_else(|| invalid("input byte count overflowed"))?;
        }
        if total > MAX_INPUT_VALUE_BYTES_TOTAL {
            return Err(invalid(
                "input manifest exceeds its aggregate value-byte limit",
            ));
        }
        let expected = canonical_digest(
            b"ostadix/execution-fabric/input-manifest/v1",
            &InputManifestMaterial {
                bindings: &self.bindings,
            },
        )?;
        if self.manifest_sha256 != expected {
            return Err(invalid("input manifest digest mismatch"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputValueKindV1 {
    Text,
    Html,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFidelityV1 {
    Structural,
    Presentation,
}

fn renderer_output_contract(
    renderer: TrustedInlineRendererV1,
) -> (OutputValueKindV1, OutputFidelityV1) {
    match renderer {
        TrustedInlineRendererV1::Html => (OutputValueKindV1::Html, OutputFidelityV1::Presentation),
        TrustedInlineRendererV1::Markdown | TrustedInlineRendererV1::Latex => {
            (OutputValueKindV1::Text, OutputFidelityV1::Presentation)
        }
        TrustedInlineRendererV1::Text => (OutputValueKindV1::Text, OutputFidelityV1::Structural),
    }
}

#[derive(Serialize)]
struct OutputContractMaterial<'a> {
    slot: &'a str,
    value_kind: OutputValueKindV1,
    fidelity: OutputFidelityV1,
    max_bytes: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct OutputContractV1 {
    pub(crate) schema: String,
    pub(crate) version: u16,
    pub(crate) slot: String,
    pub(crate) value_kind: OutputValueKindV1,
    pub(crate) fidelity: OutputFidelityV1,
    pub(crate) max_bytes: u32,
    pub(crate) contract_sha256: Sha256DigestV1,
}

impl OutputContractV1 {
    pub fn for_renderer(
        slot: impl Into<String>,
        renderer: TrustedInlineRendererV1,
        max_bytes: u32,
    ) -> Result<Self, ExecutionFabricError> {
        let slot = slot.into();
        let (value_kind, fidelity) = renderer_output_contract(renderer);
        let contract_sha256 = canonical_digest(
            b"ostadix/execution-fabric/output-contract/v1",
            &OutputContractMaterial {
                slot: &slot,
                value_kind,
                fidelity,
                max_bytes,
            },
        )?;
        let value = Self {
            schema: OUTPUT_CONTRACT_SCHEMA_V1.to_string(),
            version: WIRE_VERSION_V1,
            slot,
            value_kind,
            fidelity,
            max_bytes,
            contract_sha256,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn slot(&self) -> &str {
        &self.slot
    }

    pub fn value_kind(&self) -> OutputValueKindV1 {
        self.value_kind
    }

    pub fn fidelity(&self) -> OutputFidelityV1 {
        self.fidelity
    }

    pub fn max_bytes(&self) -> u32 {
        self.max_bytes
    }

    pub fn contract_sha256(&self) -> &Sha256DigestV1 {
        &self.contract_sha256
    }

    fn validate(&self) -> Result<(), ExecutionFabricError> {
        if self.schema != OUTPUT_CONTRACT_SCHEMA_V1 || self.version != WIRE_VERSION_V1 {
            return Err(invalid("unsupported output contract schema/version"));
        }
        validate_slot(&self.slot)?;
        if self.max_bytes == 0 || self.max_bytes > MAX_OUTPUT_BYTES {
            return Err(invalid("output contract byte limit is outside V1 bounds"));
        }
        let expected = canonical_digest(
            b"ostadix/execution-fabric/output-contract/v1",
            &OutputContractMaterial {
                slot: &self.slot,
                value_kind: self.value_kind,
                fidelity: self.fidelity,
                max_bytes: self.max_bytes,
            },
        )?;
        if self.contract_sha256 != expected {
            return Err(invalid("output contract digest mismatch"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ExecutionLimitsV1 {
    pub(crate) max_runtime_ms: u64,
    pub(crate) max_input_bytes: u32,
    pub(crate) max_output_bytes: u32,
}

impl ExecutionLimitsV1 {
    pub fn new(
        max_runtime_ms: u64,
        max_input_bytes: u32,
        max_output_bytes: u32,
    ) -> Result<Self, ExecutionFabricError> {
        let value = Self {
            max_runtime_ms,
            max_input_bytes,
            max_output_bytes,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn max_runtime_ms(&self) -> u64 {
        self.max_runtime_ms
    }

    pub fn max_input_bytes(&self) -> u32 {
        self.max_input_bytes
    }

    pub fn max_output_bytes(&self) -> u32 {
        self.max_output_bytes
    }

    fn validate(&self) -> Result<(), ExecutionFabricError> {
        if self.max_runtime_ms == 0 || self.max_runtime_ms > MAX_RUNTIME_MS {
            return Err(invalid("execution runtime limit is outside V1 bounds"));
        }
        if self.max_input_bytes == 0 || self.max_input_bytes as usize > MAX_INPUT_VALUE_BYTES_TOTAL
        {
            return Err(invalid("execution input limit is outside V1 bounds"));
        }
        if self.max_output_bytes == 0 || self.max_output_bytes > MAX_OUTPUT_BYTES {
            return Err(invalid("execution output limit is outside V1 bounds"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ExecutionCapsuleV1 {
    pub(crate) schema: String,
    pub(crate) version: u16,
    pub(crate) attempt: AttemptIdV1,
    pub(crate) region: SourceClosedRendererV1,
    pub(crate) admission_sha256: Sha256DigestV1,
    pub(crate) inputs: InputManifestV1,
    pub(crate) output: OutputContractV1,
    pub(crate) deadline_unix_ms: u64,
    pub(crate) limits: ExecutionLimitsV1,
}

impl ExecutionCapsuleV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        attempt: AttemptIdV1,
        region: SourceClosedRendererV1,
        admission_sha256: Sha256DigestV1,
        inputs: InputManifestV1,
        output: OutputContractV1,
        deadline_unix_ms: u64,
        limits: ExecutionLimitsV1,
    ) -> Result<Self, ExecutionFabricError> {
        let value = Self {
            schema: EXECUTION_CAPSULE_SCHEMA_V1.to_string(),
            version: WIRE_VERSION_V1,
            attempt,
            region,
            admission_sha256,
            inputs,
            output,
            deadline_unix_ms,
            limits,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn attempt(&self) -> &AttemptIdV1 {
        &self.attempt
    }

    pub fn region(&self) -> &SourceClosedRendererV1 {
        &self.region
    }

    pub fn inputs(&self) -> &InputManifestV1 {
        &self.inputs
    }

    pub fn output(&self) -> &OutputContractV1 {
        &self.output
    }

    pub fn deadline_unix_ms(&self) -> u64 {
        self.deadline_unix_ms
    }

    pub fn limits(&self) -> &ExecutionLimitsV1 {
        &self.limits
    }

    pub(crate) fn canonical_sha256(&self) -> Result<Sha256DigestV1, ExecutionFabricError> {
        self.validate()?;
        canonical_digest(b"ostadix/execution-fabric/capsule/v1", self)
    }

    pub(crate) fn validate(&self) -> Result<(), ExecutionFabricError> {
        if self.schema != EXECUTION_CAPSULE_SCHEMA_V1 || self.version != WIRE_VERSION_V1 {
            return Err(invalid("unsupported execution capsule schema/version"));
        }
        self.attempt.validate()?;
        self.region.validate()?;
        require_nonzero_digest("execution admission", &self.admission_sha256)?;
        self.inputs.validate()?;
        self.output.validate()?;
        self.limits.validate()?;
        if self.deadline_unix_ms == 0 {
            return Err(invalid("execution capsule deadline must be nonzero"));
        }
        let (expected_kind, expected_fidelity) = renderer_output_contract(self.region.renderer);
        if self.output.value_kind != expected_kind || self.output.fidelity != expected_fidelity {
            return Err(invalid(
                "output contract does not match the trusted renderer",
            ));
        }
        if self.output.max_bytes > self.limits.max_output_bytes {
            return Err(invalid("output contract exceeds the capsule output limit"));
        }
        let total_input_bytes =
            self.inputs
                .bindings
                .iter()
                .try_fold(0usize, |total, binding| {
                    total
                        .checked_add(binding.value.owvalue.len())
                        .ok_or_else(|| invalid("input byte count overflowed"))
                })?;
        if total_input_bytes > self.limits.max_input_bytes as usize {
            return Err(invalid("input manifest exceeds the capsule input limit"));
        }
        let referenced = self
            .region
            .parts
            .iter()
            .filter_map(|part| match part {
                RendererPartV1::Input { slot } => Some(slot.as_str()),
                RendererPartV1::Literal { .. } => None,
            })
            .collect::<BTreeSet<_>>();
        let bound = self
            .inputs
            .bindings
            .iter()
            .map(|binding| binding.slot.as_str())
            .collect::<BTreeSet<_>>();
        if referenced != bound {
            let missing = referenced.difference(&bound).copied().collect::<Vec<_>>();
            let unused = bound.difference(&referenced).copied().collect::<Vec<_>>();
            return Err(invalid(format!(
                "renderer/input manifest mismatch; missing={missing:?} unused={unused:?}"
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CandidateOutputV1 {
    pub(crate) slot: String,
    pub(crate) value: PortableValueV1,
    pub(crate) value_kind: OutputValueKindV1,
    pub(crate) fidelity: OutputFidelityV1,
}

impl CandidateOutputV1 {
    pub fn new(
        slot: impl Into<String>,
        value: &PortableValueRecord,
        value_kind: OutputValueKindV1,
        fidelity: OutputFidelityV1,
    ) -> Result<Self, ExecutionFabricError> {
        let PortableValueRecord::Core(PortableOValue::Text(_)) = value else {
            return Err(invalid(
                "V1 candidate output must be one portable Text record",
            ));
        };
        let value = Self {
            slot: slot.into(),
            value: PortableValueV1::new(value)?,
            value_kind,
            fidelity,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn value(&self) -> &PortableValueV1 {
        &self.value
    }

    pub fn slot(&self) -> &str {
        &self.slot
    }

    pub fn value_kind(&self) -> OutputValueKindV1 {
        self.value_kind
    }

    pub fn fidelity(&self) -> OutputFidelityV1 {
        self.fidelity
    }

    fn validate(&self) -> Result<(), ExecutionFabricError> {
        validate_slot(&self.slot)?;
        let record = self.value.decode()?;
        if !matches!(record, PortableValueRecord::Core(PortableOValue::Text(_))) {
            return Err(invalid("V1 candidate output must decode as portable Text"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum CandidateOutcomeV1 {
    Succeeded { output: CandidateOutputV1 },
    Failed { code: String, message: String },
}

impl CandidateOutcomeV1 {
    fn validate(&self) -> Result<(), ExecutionFabricError> {
        match self {
            Self::Succeeded { output } => output.validate(),
            Self::Failed { code, message } => {
                validate_token("candidate failure code", code)?;
                if message.is_empty() || message.len() > MAX_FAILURE_MESSAGE_BYTES {
                    return Err(invalid("candidate failure message is outside V1 bounds"));
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ExecutionCandidateV1 {
    pub(crate) schema: String,
    pub(crate) version: u16,
    pub(crate) attempt: AttemptIdV1,
    pub(crate) capsule_sha256: Sha256DigestV1,
    pub(crate) region_sha256: Sha256DigestV1,
    pub(crate) input_manifest_sha256: Sha256DigestV1,
    pub(crate) output_contract_sha256: Sha256DigestV1,
    pub(crate) outcome: CandidateOutcomeV1,
    pub(crate) completed_unix_ms: u64,
}

impl ExecutionCandidateV1 {
    pub fn new(
        capsule: &ExecutionCapsuleV1,
        outcome: CandidateOutcomeV1,
        completed_unix_ms: u64,
    ) -> Result<Self, ExecutionFabricError> {
        let value = Self {
            schema: EXECUTION_CANDIDATE_SCHEMA_V1.to_string(),
            version: WIRE_VERSION_V1,
            attempt: capsule.attempt.clone(),
            capsule_sha256: capsule.canonical_sha256()?,
            region_sha256: capsule.region.region_sha256,
            input_manifest_sha256: capsule.inputs.manifest_sha256,
            output_contract_sha256: capsule.output.contract_sha256,
            outcome,
            completed_unix_ms,
        };
        value.validate()?;
        value.validate_against(capsule)?;
        Ok(value)
    }

    pub fn outcome(&self) -> &CandidateOutcomeV1 {
        &self.outcome
    }

    pub fn validate_against(
        &self,
        capsule: &ExecutionCapsuleV1,
    ) -> Result<(), ExecutionFabricError> {
        self.validate()?;
        capsule.validate()?;
        if self.attempt != capsule.attempt
            || self.capsule_sha256 != capsule.canonical_sha256()?
            || self.region_sha256 != capsule.region.region_sha256
            || self.input_manifest_sha256 != capsule.inputs.manifest_sha256
            || self.output_contract_sha256 != capsule.output.contract_sha256
        {
            return Err(invalid(
                "candidate binding does not match the execution capsule",
            ));
        }
        if self.completed_unix_ms > capsule.deadline_unix_ms {
            return Err(invalid("candidate completed after the capsule deadline"));
        }
        match &self.outcome {
            CandidateOutcomeV1::Succeeded { output } => {
                if output.slot != capsule.output.slot
                    || output.value_kind != capsule.output.value_kind
                    || output.fidelity != capsule.output.fidelity
                    || output.value.owvalue.len() > capsule.output.max_bytes as usize
                {
                    return Err(invalid(
                        "candidate output violates its frozen output contract",
                    ));
                }
            }
            CandidateOutcomeV1::Failed { .. } => {
                return Err(invalid(
                    "the V1 trusted renderer is admitted infallible; a reported failure is an infrastructure contract violation",
                ));
            }
        }
        Ok(())
    }

    /// Validate a provisional candidate at the coordinator's acceptance clock.
    ///
    /// `completed_unix_ms` is worker-reported evidence. It cannot establish
    /// timeliness by itself. A coordinator must supply its own nonzero
    /// observation time before accepting the candidate. Successful acceptance
    /// does not publish a graph value or settle a semantic trace.
    pub fn validate_for_coordinator_acceptance(
        &self,
        capsule: &ExecutionCapsuleV1,
        coordinator_observed_unix_ms: u64,
    ) -> Result<(), ExecutionFabricError> {
        self.validate_against(capsule)?;
        if coordinator_observed_unix_ms == 0 {
            return Err(invalid(
                "coordinator acceptance observation time must be nonzero",
            ));
        }
        if coordinator_observed_unix_ms > capsule.deadline_unix_ms {
            return Err(invalid(
                "coordinator observed the candidate after the capsule deadline",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), ExecutionFabricError> {
        if self.schema != EXECUTION_CANDIDATE_SCHEMA_V1 || self.version != WIRE_VERSION_V1 {
            return Err(invalid("unsupported execution candidate schema/version"));
        }
        self.attempt.validate()?;
        for (label, digest) in [
            ("candidate capsule", &self.capsule_sha256),
            ("candidate region", &self.region_sha256),
            ("candidate input manifest", &self.input_manifest_sha256),
            ("candidate output contract", &self.output_contract_sha256),
        ] {
            require_nonzero_digest(label, digest)?;
        }
        if self.completed_unix_ms == 0 {
            return Err(invalid("candidate completion time must be nonzero"));
        }
        self.outcome.validate()
    }
}

fn validate_slot(slot: &str) -> Result<(), ExecutionFabricError> {
    validate_token("execution slot", slot)
}

fn validate_token(label: &str, token: &str) -> Result<(), ExecutionFabricError> {
    if token.is_empty()
        || token.len() > MAX_SLOT_BYTES
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid(format!("{label} is not a bounded ASCII token")));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::value::OText;

    use super::*;

    pub(crate) fn digest(seed: u8) -> Sha256DigestV1 {
        [seed; 32]
    }

    pub(crate) fn fixture_capsule() -> ExecutionCapsuleV1 {
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
            MAX_OUTPUT_BYTES,
        )
        .unwrap();
        ExecutionCapsuleV1::new(
            attempt,
            region,
            digest(7),
            inputs,
            output,
            2_000_000_000_000,
            ExecutionLimitsV1::new(30_000, 16 * 1024, MAX_OUTPUT_BYTES).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn rejects_missing_and_unused_input_bindings() {
        let mut missing = fixture_capsule();
        missing.inputs = InputManifestV1::new(Vec::new()).unwrap();
        assert!(missing
            .validate()
            .unwrap_err()
            .to_string()
            .contains("missing"));

        let extra = PortableValueRecord::Core(PortableOValue::Bool(true));
        let mut unused = fixture_capsule();
        unused.inputs = InputManifestV1::new(vec![
            InputBindingV1::new("name", &extra).unwrap(),
            InputBindingV1::new("unused", &extra).unwrap(),
        ])
        .unwrap();
        assert!(unused
            .validate()
            .unwrap_err()
            .to_string()
            .contains("unused"));
    }

    #[test]
    fn rejects_duplicate_input_slots() {
        let value = PortableValueRecord::Core(PortableOValue::Bool(true));
        let error = InputManifestV1::new(vec![
            InputBindingV1::new("same", &value).unwrap(),
            InputBindingV1::new("same", &value).unwrap(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("uniquely sorted"));
    }

    #[test]
    fn rejects_unsafe_portable_value_kinds() {
        let code_ref =
            PortableValueRecord::Core(PortableOValue::code_ref(digest(9), "o", "main").unwrap());
        assert!(PortableValueV1::new(&code_ref)
            .unwrap_err()
            .to_string()
            .contains("allowlist"));

        let tagged = PortableValueRecord::Core(
            PortableOValue::tagged("unsafe-for-renderer", PortableOValue::Null).unwrap(),
        );
        assert!(PortableValueV1::new(&tagged).is_err());

        let html_bytes = PortableValueRecord::Core(PortableOValue::Bytes(crate::value::OBytes {
            bytes: b"<script>unsafe()</script>".to_vec(),
            media_type: Some("text/html".to_string()),
        }));
        assert!(PortableValueV1::new(&html_bytes)
            .unwrap_err()
            .to_string()
            .contains("allowlist"));

        let extension = PortableValueRecord::Extension(
            crate::world::ExtensionEnvelope::new(
                "example.test",
                "unsafe",
                1,
                digest(10),
                PortableOValue::Null,
            )
            .unwrap(),
        );
        assert!(PortableValueV1::new(&extension)
            .unwrap_err()
            .to_string()
            .contains("extensions"));

        let mut too_deep = PortableOValue::Null;
        for _ in 0..=crate::world::MAX_OVALUE_DEPTH {
            too_deep = PortableOValue::List(vec![too_deep]);
        }
        assert!(PortableValueV1::new(&PortableValueRecord::Core(too_deep))
            .unwrap_err()
            .to_string()
            .contains("depth"));
    }

    #[test]
    fn rejects_zero_attempt_generation_and_deadline() {
        let task = LogicalTaskIdV1::new(ExecutionIdV1::new(digest(1)).unwrap(), digest(2)).unwrap();
        assert!(AttemptIdV1::new(task, 0).is_err());
        let mut capsule = fixture_capsule();
        capsule.deadline_unix_ms = 0;
        assert!(capsule.validate().is_err());
    }

    #[test]
    fn frozen_renderer_bounds_accept_the_exact_edge_and_reject_one_beyond() {
        let accepted_parts = vec![RendererPartV1::literal(""); MAX_RENDERER_PARTS];
        assert!(SourceClosedRendererV1::new(
            TrustedInlineRendererV1::Text,
            accepted_parts,
            digest(1),
            digest(2),
            digest(3),
            digest(4),
        )
        .is_ok());

        let rejected_parts = vec![RendererPartV1::literal(""); MAX_RENDERER_PARTS + 1];
        assert!(SourceClosedRendererV1::new(
            TrustedInlineRendererV1::Text,
            rejected_parts,
            digest(1),
            digest(2),
            digest(3),
            digest(4),
        )
        .unwrap_err()
        .to_string()
        .contains("parts"));

        assert!(SourceClosedRendererV1::new(
            TrustedInlineRendererV1::Text,
            vec![RendererPartV1::literal(
                "x".repeat(MAX_RENDERER_LITERAL_BYTES)
            )],
            digest(1),
            digest(2),
            digest(3),
            digest(4),
        )
        .is_ok());
        assert!(SourceClosedRendererV1::new(
            TrustedInlineRendererV1::Text,
            vec![RendererPartV1::literal(
                "x".repeat(MAX_RENDERER_LITERAL_BYTES + 1)
            )],
            digest(1),
            digest(2),
            digest(3),
            digest(4),
        )
        .unwrap_err()
        .to_string()
        .contains("literal"));
    }

    #[test]
    fn reported_failure_from_infallible_v1_renderer_is_infrastructure() {
        let capsule = fixture_capsule();
        let error = ExecutionCandidateV1::new(
            &capsule,
            CandidateOutcomeV1::Failed {
                code: "renderer-failed".to_string(),
                message: "unexpected renderer failure".to_string(),
            },
            capsule.deadline_unix_ms() - 1,
        )
        .unwrap_err();
        assert!(error.to_string().contains("admitted infallible"));
        assert_eq!(
            error.failure_class(),
            ExecutionFabricFailureClassV1::InfrastructureAbort
        );
    }
}
