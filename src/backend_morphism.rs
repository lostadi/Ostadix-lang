//! Bounded, shadow-mode backend morphism contracts.
//!
//! This V1 kernel describes only crossings the current adapters can actually
//! demonstrate. It is intentionally not part of backend-catalog V4 identity,
//! evidence admission, or dispatch. The HGraph solver can query the shadow
//! assessment beside its compatibility fidelity result without changing
//! execution behavior.

use std::collections::{BTreeSet, HashMap};

use num_bigint::BigInt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::registry::bundle::BackendRegistry;
use crate::value::{AnnotationKind, FidelityAssessmentV2, FloatFormat, ONumber, OText, OValue};

pub const BACKEND_MORPHISM_SCHEMA_V1: &str = "ostadix.backend-morphism/v1";
pub const MAX_BACKEND_MORPHISM_DEPTH_V1: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendMorphismProfileV1 {
    PythonPlainData,
    JavascriptBindingStdout,
    RustSourceConstantStdout,
}

impl BackendMorphismProfileV1 {
    pub const fn name(self) -> &'static str {
        match self {
            Self::PythonPlainData => "python-plain-data",
            Self::JavascriptBindingStdout => "javascript-binding-stdout",
            Self::RustSourceConstantStdout => "rust-source-constant-stdout",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendMorphismIntegrationV1 {
    Shadow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendMorphismSpecV1 {
    pub schema: &'static str,
    pub backend: &'static str,
    pub profile: BackendMorphismProfileV1,
    pub integration: BackendMorphismIntegrationV1,
    /// What `project` means for the current adapter input boundary.
    pub o_to_backend_input_boundary: &'static str,
    /// What `inject` means for an already decoded backend output. For the
    /// JavaScript and Rust profiles this is stdout egress, not a generic
    /// backend binding/injection channel.
    pub profiled_backend_output_to_o_boundary: &'static str,
}

const PYTHON_SPEC_V1: BackendMorphismSpecV1 = BackendMorphismSpecV1 {
    schema: BACKEND_MORPHISM_SCHEMA_V1,
    backend: "python",
    profile: BackendMorphismProfileV1::PythonPlainData,
    integration: BackendMorphismIntegrationV1::Shadow,
    o_to_backend_input_boundary: "current Python shim binding conversion",
    profiled_backend_output_to_o_boundary: "current Python shim result conversion",
};

const JAVASCRIPT_SPEC_V1: BackendMorphismSpecV1 = BackendMorphismSpecV1 {
    schema: BACKEND_MORPHISM_SCHEMA_V1,
    backend: "javascript",
    profile: BackendMorphismProfileV1::JavascriptBindingStdout,
    integration: BackendMorphismIntegrationV1::Shadow,
    o_to_backend_input_boundary: "current native JavaScript scalar binding preamble",
    profiled_backend_output_to_o_boundary:
        "profiled JSON/scalar stdout decoded by the native adapter",
};

const RUST_SPEC_V1: BackendMorphismSpecV1 = BackendMorphismSpecV1 {
    schema: BACKEND_MORPHISM_SCHEMA_V1,
    backend: "rust",
    profile: BackendMorphismProfileV1::RustSourceConstantStdout,
    integration: BackendMorphismIntegrationV1::Shadow,
    o_to_backend_input_boundary: "bounded generated Rust scalar source constant, not a binding",
    profiled_backend_output_to_o_boundary:
        "profiled JSON/scalar stdout decoded by the native adapter",
};

/// A finite, acyclic description of the native values considered by V1.
///
/// `Map` deliberately accepts arbitrary described keys so the kernel can
/// reject non-string maps explicitly. `Reference` represents shared/cyclic
/// object-graph edges, which are outside every V1 plain-data profile.
///
/// When passed to [`BackendMorphismV1::inject`], this describes a value already
/// acquired from the profile's declared backend-output boundary. In
/// particular, JavaScript and Rust `List`/`Map` values model profiled stdout
/// egress; they do not assert that the current adapter can bind those values as
/// backend inputs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackendNativeValueV1 {
    Null,
    Bool {
        value: bool,
    },
    Integer {
        value: BigInt,
    },
    F64 {
        bits: u64,
    },
    String {
        value: String,
    },
    List {
        items: Vec<BackendNativeValueV1>,
    },
    Map {
        entries: Vec<(BackendNativeValueV1, BackendNativeValueV1)>,
    },
    Reference {
        identity: String,
    },
    Opaque {
        type_name: String,
    },
}

impl BackendNativeValueV1 {
    pub fn f64(value: f64) -> Self {
        Self::F64 {
            bits: value.to_bits(),
        }
    }

    pub fn string(value: impl Into<String>) -> Self {
        Self::String {
            value: value.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendMorphismDirectionV1 {
    OValueToBackendInputProfile,
    ProfiledBackendOutputToOValue,
    LosslessLaw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendMorphismRejectionKindV1 {
    UnsupportedValue,
    NonStringMapKey,
    DuplicateMapKey,
    CyclicOrSharedReference,
    DepthLimit,
    IntegerOutOfRange,
    NonFiniteFloat,
    CurrentAdapterDoesNotBindContainers,
    LosslessLawViolation,
}

#[derive(Clone, Debug, Error, PartialEq, Eq, Serialize, Deserialize)]
#[error("{backend} {direction:?} rejected {path}: {message}")]
pub struct BackendMorphismErrorV1 {
    pub backend: String,
    pub direction: BackendMorphismDirectionV1,
    pub kind: BackendMorphismRejectionKindV1,
    pub path: String,
    pub message: String,
}

impl BackendMorphismErrorV1 {
    fn new(
        spec: &BackendMorphismSpecV1,
        direction: BackendMorphismDirectionV1,
        kind: BackendMorphismRejectionKindV1,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            backend: spec.backend.to_owned(),
            direction,
            kind,
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackendMorphismValueV1<T> {
    pub value: T,
    pub fidelity: FidelityAssessmentV2,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackendMorphismRoundTripV1 {
    pub projected_native: BackendNativeValueV1,
    pub reinjected_value: OValue,
    pub projection_fidelity: FidelityAssessmentV2,
    pub injection_fidelity: FidelityAssessmentV2,
    pub composed_fidelity: FidelityAssessmentV2,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BackendMorphismLegAssessmentV1 {
    Supported { fidelity: FidelityAssessmentV2 },
    Rejected { error: BackendMorphismErrorV1 },
    NotAttempted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendMorphismAssessmentV1 {
    pub schema: String,
    pub backend: String,
    pub profile: BackendMorphismProfileV1,
    pub integration: BackendMorphismIntegrationV1,
    pub o_to_backend_input_boundary: String,
    pub profiled_backend_output_to_o_boundary: String,
    pub o_to_backend_input: BackendMorphismLegAssessmentV1,
    pub profiled_backend_output_to_o: BackendMorphismLegAssessmentV1,
    pub composed_fidelity: FidelityAssessmentV2,
    /// Present only when the composed judgment claims `Lossless`.
    pub lossless_law_holds: Option<bool>,
    pub law_error: Option<BackendMorphismErrorV1>,
}

impl BackendMorphismAssessmentV1 {
    pub fn is_supported(&self) -> bool {
        !matches!(self.composed_fidelity, FidelityAssessmentV2::Unsupported)
            && self.law_error.is_none()
    }
}

/// Versioned morphism law with explicit failure and fidelity on both legs.
pub trait BackendMorphismV1 {
    fn spec(&self) -> &'static BackendMorphismSpecV1;

    /// Inject a value already acquired from the declared backend-output
    /// boundary into O.
    ///
    /// For the JavaScript and Rust profiles this consumes the native adapter's
    /// profiled stdout result. Supporting a recursive value here does not imply
    /// that the adapter can inject or bind that value into a program; that
    /// independent capability is described by [`Self::project`].
    fn inject(
        &self,
        native: &BackendNativeValueV1,
    ) -> Result<BackendMorphismValueV1<OValue>, BackendMorphismErrorV1>;

    /// Project an OValue into the bounded native profile.
    fn project(
        &self,
        value: &OValue,
    ) -> Result<BackendMorphismValueV1<BackendNativeValueV1>, BackendMorphismErrorV1>;

    fn round_trip(
        &self,
        value: &OValue,
    ) -> Result<BackendMorphismRoundTripV1, BackendMorphismErrorV1> {
        let projected = self.project(value)?;
        let injected = self.inject(&projected.value)?;
        let composed = projected.fidelity.clone().then(injected.fidelity.clone());
        if composed == FidelityAssessmentV2::Lossless && injected.value != *value {
            return Err(BackendMorphismErrorV1::new(
                self.spec(),
                BackendMorphismDirectionV1::LosslessLaw,
                BackendMorphismRejectionKindV1::LosslessLawViolation,
                "$",
                "a lossless profile did not reproduce the original OValue",
            ));
        }
        Ok(BackendMorphismRoundTripV1 {
            projected_native: projected.value,
            reinjected_value: injected.value,
            projection_fidelity: projected.fidelity,
            injection_fidelity: injected.fidelity,
            composed_fidelity: composed,
        })
    }

    /// Non-authoritative assessment consumed beside the current solver result.
    /// It never changes admission, placement, rendering, or dispatch.
    fn shadow_assess(&self, value: &OValue) -> BackendMorphismAssessmentV1 {
        let spec = self.spec();
        let projected = match self.project(value) {
            Ok(projected) => projected,
            Err(error) => {
                return BackendMorphismAssessmentV1 {
                    schema: spec.schema.to_owned(),
                    backend: spec.backend.to_owned(),
                    profile: spec.profile,
                    integration: spec.integration,
                    o_to_backend_input_boundary: spec.o_to_backend_input_boundary.to_owned(),
                    profiled_backend_output_to_o_boundary: spec
                        .profiled_backend_output_to_o_boundary
                        .to_owned(),
                    o_to_backend_input: BackendMorphismLegAssessmentV1::Rejected { error },
                    profiled_backend_output_to_o: BackendMorphismLegAssessmentV1::NotAttempted,
                    composed_fidelity: FidelityAssessmentV2::Unsupported,
                    lossless_law_holds: None,
                    law_error: None,
                };
            }
        };
        let projection_fidelity = projected.fidelity.clone();
        let injected = match self.inject(&projected.value) {
            Ok(injected) => injected,
            Err(error) => {
                return BackendMorphismAssessmentV1 {
                    schema: spec.schema.to_owned(),
                    backend: spec.backend.to_owned(),
                    profile: spec.profile,
                    integration: spec.integration,
                    o_to_backend_input_boundary: spec.o_to_backend_input_boundary.to_owned(),
                    profiled_backend_output_to_o_boundary: spec
                        .profiled_backend_output_to_o_boundary
                        .to_owned(),
                    o_to_backend_input: BackendMorphismLegAssessmentV1::Supported {
                        fidelity: projection_fidelity,
                    },
                    profiled_backend_output_to_o: BackendMorphismLegAssessmentV1::Rejected {
                        error,
                    },
                    composed_fidelity: FidelityAssessmentV2::Unsupported,
                    lossless_law_holds: None,
                    law_error: None,
                };
            }
        };
        let composed_fidelity = projection_fidelity.clone().then(injected.fidelity.clone());
        let lossless_law_holds = (composed_fidelity == FidelityAssessmentV2::Lossless)
            .then_some(injected.value == *value);
        let law_error = matches!(lossless_law_holds, Some(false)).then(|| {
            BackendMorphismErrorV1::new(
                spec,
                BackendMorphismDirectionV1::LosslessLaw,
                BackendMorphismRejectionKindV1::LosslessLawViolation,
                "$",
                "a lossless profile did not reproduce the original OValue",
            )
        });
        let composed_fidelity = if law_error.is_some() {
            FidelityAssessmentV2::Unsupported
        } else {
            composed_fidelity
        };
        BackendMorphismAssessmentV1 {
            schema: spec.schema.to_owned(),
            backend: spec.backend.to_owned(),
            profile: spec.profile,
            integration: spec.integration,
            o_to_backend_input_boundary: spec.o_to_backend_input_boundary.to_owned(),
            profiled_backend_output_to_o_boundary: spec
                .profiled_backend_output_to_o_boundary
                .to_owned(),
            o_to_backend_input: BackendMorphismLegAssessmentV1::Supported {
                fidelity: projection_fidelity,
            },
            profiled_backend_output_to_o: BackendMorphismLegAssessmentV1::Supported {
                fidelity: injected.fidelity,
            },
            composed_fidelity,
            lossless_law_holds,
            law_error,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendMorphismKernelV1 {
    Python,
    Javascript,
    Rust,
}

impl BackendMorphismKernelV1 {
    /// Resolve aliases through the canonical backend catalog, while keeping
    /// these shadow profiles outside the catalog's V4 identity.
    pub fn for_backend(tag: &str) -> Option<Self> {
        match BackendRegistry::global().get(tag)?.name {
            "python" => Some(Self::Python),
            "javascript" => Some(Self::Javascript),
            "rust" => Some(Self::Rust),
            _ => None,
        }
    }
}

impl BackendMorphismV1 for BackendMorphismKernelV1 {
    fn spec(&self) -> &'static BackendMorphismSpecV1 {
        match self {
            Self::Python => &PYTHON_SPEC_V1,
            Self::Javascript => &JAVASCRIPT_SPEC_V1,
            Self::Rust => &RUST_SPEC_V1,
        }
    }

    fn inject(
        &self,
        native: &BackendNativeValueV1,
    ) -> Result<BackendMorphismValueV1<OValue>, BackendMorphismErrorV1> {
        inject_native(self.spec(), native, 0, "$".to_owned())
    }

    fn project(
        &self,
        value: &OValue,
    ) -> Result<BackendMorphismValueV1<BackendNativeValueV1>, BackendMorphismErrorV1> {
        project_ovalue(self.spec(), value, 0, "$".to_owned())
    }
}

pub fn shadow_assess_backend_morphism_v1(
    backend: &str,
    value: &OValue,
) -> Option<BackendMorphismAssessmentV1> {
    BackendMorphismKernelV1::for_backend(backend).map(|kernel| kernel.shadow_assess(value))
}

/// Render the exact standalone Rust program for the bounded V1 source-constant
/// input profile.
///
/// This is deliberately separate from [`BackendMorphismV1::inject`]: it turns
/// the native value produced by the Rust `project` leg into executable source.
/// Recursive containers are not accepted. The returned program prints one
/// scalar through the stdout boundary reported by the Rust kernel's
/// [`BackendMorphismV1::spec`].
pub fn render_rust_scalar_stdout_program_v1(
    native: &BackendNativeValueV1,
) -> Result<String, BackendMorphismErrorV1> {
    let spec = &RUST_SPEC_V1;
    let body = match native {
        BackendNativeValueV1::Null => {
            "    let value: Option<()> = None;\n    if value.is_none() { println!(\"null\"); }\n"
                .to_owned()
        }
        BackendNativeValueV1::Bool { value } => {
            format!("    let value: bool = {value};\n    println!(\"{{value}}\");\n")
        }
        BackendNativeValueV1::Integer { value } => {
            validate_integer(
                spec,
                value,
                BackendMorphismDirectionV1::OValueToBackendInputProfile,
                "$",
            )?;
            let literal = if value == &BigInt::from(i128::MIN) {
                "i128::MIN".to_owned()
            } else {
                format!("{value}_i128")
            };
            format!("    let value: i128 = {literal};\n    println!(\"{{value}}\");\n")
        }
        BackendNativeValueV1::F64 { bits } => {
            validate_float(
                spec,
                f64::from_bits(*bits),
                BackendMorphismDirectionV1::OValueToBackendInputProfile,
                "$",
            )?;
            format!(
                "    let value: f64 = f64::from_bits({bits}_u64);\n    println!(\"{{value:?}}\");\n"
            )
        }
        BackendNativeValueV1::String { value } => {
            let literal = rust_string_literal_v1(value);
            format!(
                r####"    let value: &str = {literal};
    print!("\"");
    for character in value.chars() {{
        match character {{
            '"' => print!("\\\""),
            '\\' => print!("\\\\"),
            '\u{{0008}}' => print!("\\b"),
            '\u{{000c}}' => print!("\\f"),
            '\n' => print!("\\n"),
            '\r' => print!("\\r"),
            '\t' => print!("\\t"),
            character if character <= '\u{{001f}}' => {{
                print!("\\u{{:04x}}", character as u32);
            }}
            character => print!("{{character}}"),
        }}
    }}
    println!("\"");
"####
            )
        }
        BackendNativeValueV1::List { .. } | BackendNativeValueV1::Map { .. } => {
            return Err(BackendMorphismErrorV1::new(
                spec,
                BackendMorphismDirectionV1::OValueToBackendInputProfile,
                BackendMorphismRejectionKindV1::CurrentAdapterDoesNotBindContainers,
                "$",
                "the bounded Rust V1 source renderer accepts scalar constants only",
            ));
        }
        BackendNativeValueV1::Reference { identity } => {
            return Err(BackendMorphismErrorV1::new(
                spec,
                BackendMorphismDirectionV1::OValueToBackendInputProfile,
                BackendMorphismRejectionKindV1::CyclicOrSharedReference,
                "$",
                format!("Rust source constant cannot encode reference {identity:?}"),
            ));
        }
        BackendNativeValueV1::Opaque { type_name } => {
            return Err(BackendMorphismErrorV1::new(
                spec,
                BackendMorphismDirectionV1::OValueToBackendInputProfile,
                BackendMorphismRejectionKindV1::UnsupportedValue,
                "$",
                format!("Rust source constant cannot encode native type {type_name:?}"),
            ));
        }
    };
    Ok(format!("fn main() {{\n{body}}}\n"))
}

fn rust_string_literal_v1(value: &str) -> String {
    let mut literal = String::with_capacity(value.len() + 2);
    literal.push('"');
    for character in value.chars() {
        literal.extend(character.escape_default());
    }
    literal.push('"');
    literal
}

fn inject_native(
    spec: &BackendMorphismSpecV1,
    native: &BackendNativeValueV1,
    depth: usize,
    path: String,
) -> Result<BackendMorphismValueV1<OValue>, BackendMorphismErrorV1> {
    check_depth(
        spec,
        BackendMorphismDirectionV1::ProfiledBackendOutputToOValue,
        depth,
        &path,
    )?;
    let outcome = match native {
        BackendNativeValueV1::Null => BackendMorphismValueV1 {
            value: OValue::Null,
            fidelity: FidelityAssessmentV2::Lossless,
        },
        BackendNativeValueV1::Bool { value } => BackendMorphismValueV1 {
            value: OValue::bool_(*value),
            fidelity: FidelityAssessmentV2::Lossless,
        },
        BackendNativeValueV1::Integer { value } => {
            validate_integer(
                spec,
                value,
                BackendMorphismDirectionV1::ProfiledBackendOutputToOValue,
                &path,
            )?;
            BackendMorphismValueV1 {
                value: OValue::big_int(value.clone()),
                fidelity: numeric_fidelity(spec.profile),
            }
        }
        BackendNativeValueV1::F64 { bits } => {
            let value = f64::from_bits(*bits);
            validate_float(
                spec,
                value,
                BackendMorphismDirectionV1::ProfiledBackendOutputToOValue,
                &path,
            )?;
            BackendMorphismValueV1 {
                value: OValue::float(value),
                fidelity: numeric_fidelity(spec.profile),
            }
        }
        BackendNativeValueV1::String { value } => BackendMorphismValueV1 {
            value: OValue::text(value.clone()),
            fidelity: FidelityAssessmentV2::Lossless,
        },
        BackendNativeValueV1::List { items } => {
            let mut values = Vec::with_capacity(items.len());
            let mut fidelity = FidelityAssessmentV2::Lossless;
            for (index, item) in items.iter().enumerate() {
                let item = inject_native(spec, item, depth + 1, format!("{path}[{index}]"))?;
                fidelity = fidelity.then(item.fidelity);
                values.push(item.value);
            }
            BackendMorphismValueV1 {
                value: OValue::list(values),
                fidelity,
            }
        }
        BackendNativeValueV1::Map { entries } => {
            let mut values = HashMap::with_capacity(entries.len());
            let mut fidelity = FidelityAssessmentV2::Lossless;
            for (index, (key, value)) in entries.iter().enumerate() {
                let BackendNativeValueV1::String { value: key } = key else {
                    return Err(BackendMorphismErrorV1::new(
                        spec,
                        BackendMorphismDirectionV1::ProfiledBackendOutputToOValue,
                        BackendMorphismRejectionKindV1::NonStringMapKey,
                        format!("{path}.key[{index}]"),
                        "V1 plain-data maps require native string keys",
                    ));
                };
                if values.contains_key(key) {
                    return Err(BackendMorphismErrorV1::new(
                        spec,
                        BackendMorphismDirectionV1::ProfiledBackendOutputToOValue,
                        BackendMorphismRejectionKindV1::DuplicateMapKey,
                        format!("{path}.key[{index}]"),
                        "V1 plain-data maps cannot collapse duplicate string keys",
                    ));
                }
                let value = inject_native(spec, value, depth + 1, format!("{path}.{key}"))?;
                fidelity = fidelity.then(value.fidelity);
                values.insert(key.clone(), value.value);
            }
            BackendMorphismValueV1 {
                value: OValue::map(values),
                fidelity,
            }
        }
        BackendNativeValueV1::Reference { identity } => {
            return Err(BackendMorphismErrorV1::new(
                spec,
                BackendMorphismDirectionV1::ProfiledBackendOutputToOValue,
                BackendMorphismRejectionKindV1::CyclicOrSharedReference,
                path,
                format!(
                    "native reference {identity:?} requires an identity/cycle-aware graph codec"
                ),
            ));
        }
        BackendNativeValueV1::Opaque { type_name } => {
            return Err(BackendMorphismErrorV1::new(
                spec,
                BackendMorphismDirectionV1::ProfiledBackendOutputToOValue,
                BackendMorphismRejectionKindV1::UnsupportedValue,
                path,
                format!("native value type {type_name:?} is outside this V1 profile"),
            ));
        }
    };
    Ok(outcome)
}

fn project_ovalue(
    spec: &BackendMorphismSpecV1,
    value: &OValue,
    depth: usize,
    path: String,
) -> Result<BackendMorphismValueV1<BackendNativeValueV1>, BackendMorphismErrorV1> {
    check_depth(
        spec,
        BackendMorphismDirectionV1::OValueToBackendInputProfile,
        depth,
        &path,
    )?;
    let outcome = match value {
        OValue::Null => BackendMorphismValueV1 {
            value: BackendNativeValueV1::Null,
            fidelity: FidelityAssessmentV2::Lossless,
        },
        OValue::Bool { v } => BackendMorphismValueV1 {
            value: BackendNativeValueV1::Bool { value: *v },
            fidelity: FidelityAssessmentV2::Lossless,
        },
        OValue::Number { v } => project_number(spec, v, &path)?,
        OValue::Text {
            v: OText { utf8, encoding },
        } => BackendMorphismValueV1 {
            value: BackendNativeValueV1::string(utf8.clone()),
            fidelity: if encoding.as_deref() == Some("utf-8") {
                FidelityAssessmentV2::Lossless
            } else {
                concrete_structural([AnnotationKind::Encoding])
            },
        },
        OValue::List { v } if spec.profile == BackendMorphismProfileV1::PythonPlainData => {
            let mut items = Vec::with_capacity(v.len());
            let mut fidelity = FidelityAssessmentV2::Lossless;
            for (index, value) in v.iter().enumerate() {
                let value = project_ovalue(spec, value, depth + 1, format!("{path}[{index}]"))?;
                fidelity = fidelity.then(value.fidelity);
                items.push(value.value);
            }
            BackendMorphismValueV1 {
                value: BackendNativeValueV1::List { items },
                fidelity,
            }
        }
        OValue::Map { v } if spec.profile == BackendMorphismProfileV1::PythonPlainData => {
            let mut keys = v.keys().collect::<Vec<_>>();
            keys.sort();
            let mut entries = Vec::with_capacity(keys.len());
            let mut fidelity = FidelityAssessmentV2::Lossless;
            for key in keys {
                let value = project_ovalue(spec, &v[key], depth + 1, format!("{path}.{key}"))?;
                fidelity = fidelity.then(value.fidelity);
                entries.push((BackendNativeValueV1::string(key.clone()), value.value));
            }
            BackendMorphismValueV1 {
                value: BackendNativeValueV1::Map { entries },
                fidelity,
            }
        }
        OValue::List { .. } | OValue::Map { .. }
            if matches!(
                spec.profile,
                BackendMorphismProfileV1::JavascriptBindingStdout
                    | BackendMorphismProfileV1::RustSourceConstantStdout
            ) =>
        {
            return Err(BackendMorphismErrorV1::new(
                spec,
                BackendMorphismDirectionV1::OValueToBackendInputProfile,
                BackendMorphismRejectionKindV1::CurrentAdapterDoesNotBindContainers,
                path,
                match spec.profile {
                    BackendMorphismProfileV1::JavascriptBindingStdout => {
                        "the current JavaScript preamble does not establish native recursive container bindings"
                    }
                    BackendMorphismProfileV1::RustSourceConstantStdout => {
                        "the current Rust shim has no generic binding channel; V1 source constants are scalar only"
                    }
                    BackendMorphismProfileV1::PythonPlainData => unreachable!(),
                },
            ));
        }
        other => {
            return Err(BackendMorphismErrorV1::new(
                spec,
                BackendMorphismDirectionV1::OValueToBackendInputProfile,
                BackendMorphismRejectionKindV1::UnsupportedValue,
                path,
                format!(
                    "OValue kind {:?} is outside this V1 profile",
                    other.type_name()
                ),
            ));
        }
    };
    Ok(outcome)
}

fn project_number(
    spec: &BackendMorphismSpecV1,
    number: &ONumber,
    path: &str,
) -> Result<BackendMorphismValueV1<BackendNativeValueV1>, BackendMorphismErrorV1> {
    let value = match number {
        ONumber::Int { v } => {
            validate_integer(
                spec,
                v,
                BackendMorphismDirectionV1::OValueToBackendInputProfile,
                path,
            )?;
            BackendNativeValueV1::Integer { value: v.clone() }
        }
        ONumber::BinaryFloat {
            format: FloatFormat::F64,
            bits,
        } if bits.len() == 8 => {
            let mut raw = [0_u8; 8];
            raw.copy_from_slice(bits);
            let bits = u64::from_be_bytes(raw);
            validate_float(
                spec,
                f64::from_bits(bits),
                BackendMorphismDirectionV1::OValueToBackendInputProfile,
                path,
            )?;
            BackendNativeValueV1::F64 { bits }
        }
        _ => {
            return Err(BackendMorphismErrorV1::new(
                spec,
                BackendMorphismDirectionV1::OValueToBackendInputProfile,
                BackendMorphismRejectionKindV1::UnsupportedValue,
                path,
                "V1 accepts only integers and exact f64-bit scalar values",
            ));
        }
    };
    Ok(BackendMorphismValueV1 {
        value,
        fidelity: numeric_fidelity(spec.profile),
    })
}

fn check_depth(
    spec: &BackendMorphismSpecV1,
    direction: BackendMorphismDirectionV1,
    depth: usize,
    path: &str,
) -> Result<(), BackendMorphismErrorV1> {
    if depth <= MAX_BACKEND_MORPHISM_DEPTH_V1 {
        return Ok(());
    }
    Err(BackendMorphismErrorV1::new(
        spec,
        direction,
        BackendMorphismRejectionKindV1::DepthLimit,
        path,
        format!("plain-data nesting exceeds the V1 limit of {MAX_BACKEND_MORPHISM_DEPTH_V1}"),
    ))
}

fn validate_integer(
    spec: &BackendMorphismSpecV1,
    value: &BigInt,
    direction: BackendMorphismDirectionV1,
    path: &str,
) -> Result<(), BackendMorphismErrorV1> {
    let in_range = match spec.profile {
        BackendMorphismProfileV1::PythonPlainData => true,
        BackendMorphismProfileV1::JavascriptBindingStdout => {
            let bound = BigInt::from(1_u8) << 53_usize;
            value >= &-&bound && value <= &bound
        }
        BackendMorphismProfileV1::RustSourceConstantStdout => {
            value >= &BigInt::from(i128::MIN) && value <= &BigInt::from(i128::MAX)
        }
    };
    if in_range {
        return Ok(());
    }
    Err(BackendMorphismErrorV1::new(
        spec,
        direction,
        BackendMorphismRejectionKindV1::IntegerOutOfRange,
        path,
        match spec.profile {
            BackendMorphismProfileV1::PythonPlainData => unreachable!(),
            BackendMorphismProfileV1::JavascriptBindingStdout => {
                "integer is outside JavaScript's consecutive [-2^53, 2^53] Number range"
            }
            BackendMorphismProfileV1::RustSourceConstantStdout => {
                "integer is outside the bounded Rust i128 source-constant profile"
            }
        },
    ))
}

fn validate_float(
    spec: &BackendMorphismSpecV1,
    value: f64,
    direction: BackendMorphismDirectionV1,
    path: &str,
) -> Result<(), BackendMorphismErrorV1> {
    if spec.profile == BackendMorphismProfileV1::PythonPlainData || value.is_finite() {
        return Ok(());
    }
    Err(BackendMorphismErrorV1::new(
        spec,
        direction,
        BackendMorphismRejectionKindV1::NonFiniteFloat,
        path,
        "current JSON/scalar stdout profile does not preserve non-finite f64 values",
    ))
}

fn numeric_fidelity(profile: BackendMorphismProfileV1) -> FidelityAssessmentV2 {
    match profile {
        BackendMorphismProfileV1::PythonPlainData => FidelityAssessmentV2::Lossless,
        BackendMorphismProfileV1::JavascriptBindingStdout
        | BackendMorphismProfileV1::RustSourceConstantStdout => {
            concrete_structural([AnnotationKind::NumericExactness, AnnotationKind::TypeTag])
        }
    }
}

fn concrete_structural(losses: impl IntoIterator<Item = AnnotationKind>) -> FidelityAssessmentV2 {
    let losses = losses.into_iter().collect::<BTreeSet<_>>();
    FidelityAssessmentV2::structural(losses.clone(), losses)
        .expect("identical fidelity bounds preserve the subset invariant")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: Vec<(BackendNativeValueV1, BackendNativeValueV1)>) -> BackendNativeValueV1 {
        BackendNativeValueV1::Map { entries }
    }

    #[test]
    fn canonical_alias_resolves_without_extending_catalog_identity() {
        let python = BackendMorphismKernelV1::for_backend("py").unwrap();
        assert_eq!(python.spec(), &PYTHON_SPEC_V1);
        assert_eq!(
            python.spec().integration,
            BackendMorphismIntegrationV1::Shadow
        );
        assert!(BackendMorphismKernelV1::for_backend("html").is_none());
    }

    #[test]
    fn python_plain_data_round_trip_is_recursive_and_lossless() {
        let value = OValue::map(HashMap::from([(
            "items".to_owned(),
            OValue::list(vec![OValue::int(7), OValue::bool_(true), OValue::Null]),
        )]));
        let round_trip = BackendMorphismKernelV1::Python.round_trip(&value).unwrap();
        assert_eq!(round_trip.reinjected_value, value);
        assert_eq!(round_trip.composed_fidelity, FidelityAssessmentV2::Lossless);
    }

    #[test]
    fn native_cycles_non_string_keys_duplicates_and_opaque_values_are_rejected() {
        let python = BackendMorphismKernelV1::Python;
        let cases = [
            (
                BackendNativeValueV1::Reference {
                    identity: "cycle-0".to_owned(),
                },
                BackendMorphismRejectionKindV1::CyclicOrSharedReference,
            ),
            (
                map(vec![(
                    BackendNativeValueV1::Integer {
                        value: BigInt::from(1),
                    },
                    BackendNativeValueV1::Null,
                )]),
                BackendMorphismRejectionKindV1::NonStringMapKey,
            ),
            (
                map(vec![
                    (
                        BackendNativeValueV1::string("same"),
                        BackendNativeValueV1::Null,
                    ),
                    (
                        BackendNativeValueV1::string("same"),
                        BackendNativeValueV1::Bool { value: true },
                    ),
                ]),
                BackendMorphismRejectionKindV1::DuplicateMapKey,
            ),
            (
                BackendNativeValueV1::Opaque {
                    type_name: "module.Widget".to_owned(),
                },
                BackendMorphismRejectionKindV1::UnsupportedValue,
            ),
        ];
        for (value, expected) in cases {
            let error = python.inject(&value).unwrap_err();
            assert_eq!(error.kind, expected);
            assert_eq!(
                error.direction,
                BackendMorphismDirectionV1::ProfiledBackendOutputToOValue
            );
        }

        let mut too_deep = BackendNativeValueV1::Null;
        for _ in 0..=MAX_BACKEND_MORPHISM_DEPTH_V1 {
            too_deep = BackendNativeValueV1::List {
                items: vec![too_deep],
            };
        }
        assert_eq!(
            python.inject(&too_deep).unwrap_err().kind,
            BackendMorphismRejectionKindV1::DepthLimit
        );
    }

    #[test]
    fn javascript_and_rust_do_not_claim_generic_container_bindings() {
        let value = OValue::list(vec![OValue::int(1)]);
        for kernel in [
            BackendMorphismKernelV1::Javascript,
            BackendMorphismKernelV1::Rust,
        ] {
            let assessment = kernel.shadow_assess(&value);
            assert!(!assessment.is_supported());
            assert_eq!(
                assessment.profiled_backend_output_to_o_boundary,
                "profiled JSON/scalar stdout decoded by the native adapter"
            );
            let BackendMorphismLegAssessmentV1::Rejected { error } = assessment.o_to_backend_input
            else {
                panic!("container projection was not rejected")
            };
            assert_eq!(
                error.kind,
                BackendMorphismRejectionKindV1::CurrentAdapterDoesNotBindContainers
            );
            assert_eq!(
                error.direction,
                BackendMorphismDirectionV1::OValueToBackendInputProfile
            );
        }

        let render_error = render_rust_scalar_stdout_program_v1(&BackendNativeValueV1::List {
            items: vec![BackendNativeValueV1::Null],
        })
        .unwrap_err();
        assert_eq!(
            render_error.kind,
            BackendMorphismRejectionKindV1::CurrentAdapterDoesNotBindContainers
        );
        assert_eq!(
            render_error.direction,
            BackendMorphismDirectionV1::OValueToBackendInputProfile
        );
    }

    #[test]
    fn numeric_loss_is_explicit_and_composes() {
        let value = OValue::int(42);
        let assessment = BackendMorphismKernelV1::Javascript.shadow_assess(&value);
        let FidelityAssessmentV2::Structural { definite, possible } = assessment.composed_fidelity
        else {
            panic!("JavaScript numeric crossing did not expose structural loss")
        };
        let definite = definite.expect("numeric loss must be definite");
        for loss in [AnnotationKind::NumericExactness, AnnotationKind::TypeTag] {
            assert!(definite.contains(&loss));
            assert!(possible.contains(&loss));
        }
    }

    #[test]
    fn javascript_large_integer_rust_big_integer_and_nonfinite_stdout_are_rejected() {
        let javascript = OValue::big_int((BigInt::from(1_u8) << 53_usize) + 1_u8);
        assert_eq!(
            BackendMorphismKernelV1::Javascript
                .project(&javascript)
                .unwrap_err()
                .kind,
            BackendMorphismRejectionKindV1::IntegerOutOfRange
        );
        let rust = OValue::big_int(BigInt::from(i128::MAX) + 1_u8);
        assert_eq!(
            BackendMorphismKernelV1::Rust
                .project(&rust)
                .unwrap_err()
                .kind,
            BackendMorphismRejectionKindV1::IntegerOutOfRange
        );
        assert_eq!(
            BackendMorphismKernelV1::Javascript
                .project(&OValue::float(f64::NAN))
                .unwrap_err()
                .kind,
            BackendMorphismRejectionKindV1::NonFiniteFloat
        );
    }

    #[test]
    fn encoding_annotation_loss_is_not_silently_lossless() {
        let encoded = OValue::text_with_encoding("hello", Some("utf-16".to_owned()));
        let assessment = BackendMorphismKernelV1::Python.shadow_assess(&encoded);
        let FidelityAssessmentV2::Structural { definite, possible } = assessment.composed_fidelity
        else {
            panic!("encoding loss was not structural")
        };
        assert!(definite
            .expect("encoding loss must be definite")
            .contains(&AnnotationKind::Encoding));
        assert!(possible.contains(&AnnotationKind::Encoding));
    }
}
