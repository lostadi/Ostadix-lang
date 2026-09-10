//! Canonical compiled backend catalog and exact implementation metadata.
//!
//! This module is the one source of truth for backend aliases, adapters,
//! runtime requirements, value fidelity, state support, bounded morphism-profile
//! assignment, and catalog identity.
//! The placement protocol depends only on an injected catalog interface; this
//! module supplies the process-wide current implementation. Public
//! compatibility paths are projections of this single module identity.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, OnceLock};

use num_bigint::BigInt;
use sha2::{Digest, Sha256};

use crate::placement_protocol::{
    BackendImplementationIdV1, BackendStateSupportV2, CurrentBackendCatalogV1,
    PlacementValidationError, SemanticDigestV1, SnapshotCompatibilityV2,
};
use crate::resource_identity::ArtifactId;
use crate::syntax_dialect::SyntaxDialect;
use crate::value::BackendAuthority;

/// Wire ABI spoken by the current local evaluator/backend process boundary.
pub const LOCAL_BACKEND_PROTOCOL_ABI_V1: &str = "o-backend-cbor-v1";
/// Archival local-realization material schema. V1 identities remain decodable
/// but are not current placement authority.
pub const LOCAL_REALIZATION_SCHEMA_V1: &str = "ostadix.local-realization/v1";
/// Archival V1 realization digest domain.
pub const LOCAL_REALIZATION_DIGEST_DOMAIN_V1: &str = "ostadix/registry/local-realization/v1";
/// Current local-realization material schema.
pub const LOCAL_REALIZATION_SCHEMA_V2: &str = "ostadix.local-realization/v2";
/// Current domain separating realization-pipeline digests from legacy V1.
pub const LOCAL_REALIZATION_DIGEST_DOMAIN_V2: &str = "ostadix/registry/local-realization/v2";
/// Current semantic executable-set domain. Path-bearing V1 discovery digests
/// are archival coordinates and never authorize a current placement.
pub const BACKEND_EXECUTABLE_SET_DIGEST_DOMAIN_V2: &str = "ostadix/backend-executable-set/v2";

/// How one exact direct-launch alternative was selected for a backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendExecutableSelectionV2 {
    CompleteCatalogAlternative,
    AdapterDirectLauncherRefinement,
}

impl BackendExecutableSelectionV2 {
    pub const fn name(self) -> &'static str {
        match self {
            Self::CompleteCatalogAlternative => "complete-catalog-alternative",
            Self::AdapterDirectLauncherRefinement => "adapter-direct-launcher-refinement",
        }
    }
}

/// Path-independent semantic coordinate for one executable consumed by a
/// backend launch. Physical path and file identities stay in admission
/// evidence; this row binds the selected catalog alternative, invocation
/// meaning, role, and immutable content.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct BackendExecutableSetRowV2 {
    requirement_key: String,
    selected_alternative: u32,
    selection: BackendExecutableSelectionV2,
    logical_command: String,
    role: String,
    artifact: ArtifactId,
}

impl BackendExecutableSetRowV2 {
    pub fn new(
        requirement_key: impl Into<String>,
        selected_alternative: u32,
        selection: BackendExecutableSelectionV2,
        logical_command: impl Into<String>,
        role: impl Into<String>,
        artifact: ArtifactId,
    ) -> Result<Self, PlacementValidationError> {
        let requirement_key = requirement_key.into();
        let logical_command = logical_command.into();
        let role = role.into();
        validate_executable_set_token("backend executable requirement key", &requirement_key)?;
        validate_executable_set_token("backend executable logical command", &logical_command)?;
        validate_executable_set_token("backend executable role", &role)?;
        if !matches!(
            role.as_str(),
            "direct-launcher" | "ostadix-proxy" | "sandbox-wrapper"
        ) {
            return Err(PlacementValidationError::InvalidToken {
                field: "backend executable role",
                value: role,
            });
        }
        Ok(Self {
            requirement_key,
            selected_alternative,
            selection,
            logical_command,
            role,
            artifact,
        })
    }

    pub fn requirement_key(&self) -> &str {
        &self.requirement_key
    }

    pub const fn selected_alternative(&self) -> u32 {
        self.selected_alternative
    }

    pub const fn selection(&self) -> BackendExecutableSelectionV2 {
        self.selection
    }

    pub fn logical_command(&self) -> &str {
        &self.logical_command
    }

    pub fn role(&self) -> &str {
        &self.role
    }

    pub fn artifact(&self) -> &ArtifactId {
        &self.artifact
    }

    fn coordinate_label(&self) -> String {
        format!(
            "{}@{}/{}/{}/{}",
            self.requirement_key,
            self.selected_alternative,
            self.selection.name(),
            self.logical_command,
            self.role
        )
    }
}

/// Project exact launch rows into the current path-independent executable-set
/// identity. Inputs are sorted canonically, so filesystem traversal and
/// manifest row order cannot affect the result.
pub fn backend_executable_set_v2(
    rows: impl IntoIterator<Item = BackendExecutableSetRowV2>,
) -> Result<SemanticDigestV1, PlacementValidationError> {
    let mut rows = rows.into_iter().collect::<Vec<_>>();
    rows.sort();

    let mut coordinates = BTreeSet::new();
    for row in &rows {
        let coordinate = (
            row.requirement_key.as_str(),
            row.selected_alternative,
            row.selection,
            row.logical_command.as_str(),
            row.role.as_str(),
        );
        if !coordinates.insert(coordinate) {
            return Err(PlacementValidationError::Duplicate {
                kind: "backend executable-set coordinate",
                value: row.coordinate_label(),
            });
        }
    }
    if let Some(first) = rows.first() {
        if rows.iter().skip(1).any(|row| {
            row.requirement_key != first.requirement_key
                || row.selected_alternative != first.selected_alternative
                || row.selection != first.selection
        }) {
            return Err(PlacementValidationError::InvalidToken {
                field: "backend executable-set selection",
                value: "mixed requirement or alternative coordinates".to_owned(),
            });
        }
    }

    let bytes = serde_json::to_vec(&rows)
        .map_err(|error| PlacementValidationError::CanonicalSerialization(error.to_string()))?;
    Ok(SemanticDigestV1::hash_bytes(
        BACKEND_EXECUTABLE_SET_DIGEST_DOMAIN_V2,
        &bytes,
    ))
}

fn validate_executable_set_token(
    field: &'static str,
    value: &str,
) -> Result<(), PlacementValidationError> {
    const MAX_TOKEN_BYTES: usize = 128;
    if value.is_empty() {
        return Err(PlacementValidationError::Empty { field });
    }
    if value.len() > MAX_TOKEN_BYTES {
        return Err(PlacementValidationError::TooLong {
            field,
            limit: MAX_TOKEN_BYTES,
        });
    }
    if matches!(value, "." | "..")
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+' | b':' | b'/')
        })
    {
        return Err(PlacementValidationError::InvalidToken {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    InlineAst,
    InlineValue,
    Shim,
}

impl ExecutionMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ExecutionMode::InlineAst => "inline_ast",
            ExecutionMode::InlineValue => "inline_value",
            ExecutionMode::Shim => "shim",
        }
    }
}

/// Integer interval a backend can preserve exactly when an O number crosses
/// into that backend. This is semantic capability metadata, not an ISA or
/// language-name heuristic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegerExactness {
    /// The catalog has no sound positive statement for this implementation.
    Unknown,
    /// Every integer in `[-2^bits, 2^bits]` is represented exactly.
    ExactMagnitudeBits(u16),
    /// Every integer in `[-2^bits, 2^bits - 1]` is represented exactly.
    ///
    /// `bits` is the magnitude exponent, so a signed 64-bit representation is
    /// `TwosComplementBits(63)`.
    TwosComplementBits(u16),
    /// An arbitrary inclusive exact interval. Catalog declarations use signed
    /// base-10 literals which are parsed into canonical `BigInt` bounds.
    ExactRange { min: BigInt, max: BigInt },
    /// Integer precision is unbounded for the hosted representation.
    Arbitrary,
}

impl IntegerExactness {
    const fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::ExactMagnitudeBits(_) => "exact-magnitude-bits",
            Self::TwosComplementBits(_) => "twos-complement-bits",
            Self::ExactRange { .. } => "exact-range",
            Self::Arbitrary => "arbitrary",
        }
    }
}

/// Whether a backend preserves O's distinct numeric kinds rather than
/// collapsing them into a narrower scalar representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RichNumberPreservation {
    Unknown,
    Preserved,
    Collapsed,
}

impl RichNumberPreservation {
    const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Preserved => "preserved",
            Self::Collapsed => "collapsed",
        }
    }
}

/// Value capabilities frozen into a backend interface at lowering time.
/// Unknown facts stay explicit so fidelity and placement analysis cannot turn
/// an absent catalog statement into `Lossless` evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendValueCapabilities {
    pub integer_exactness: IntegerExactness,
    pub rich_numbers: RichNumberPreservation,
}

impl BackendValueCapabilities {
    pub const UNKNOWN: Self = Self {
        integer_exactness: IntegerExactness::Unknown,
        rich_numbers: RichNumberPreservation::Unknown,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInterface {
    pub canonical: String,
    /// Digest of the canonical catalog entry, including value capabilities.
    /// `None` denotes a compatibility backend absent from the catalog.
    pub specification_sha256: Option<String>,
    pub pure: bool,
    pub renderer: SpliceRenderer,
    pub execution: ExecutionMode,
    pub value_capabilities: BackendValueCapabilities,
    /// State behavior of this exact current catalog entry. `None` means the
    /// tag is an uncatalogued compatibility backend and cannot authorize a
    /// stateful placement or migration claim.
    pub state_support: Option<BackendStateSupportV2>,
    /// Authority required by the backend adapter itself, before any
    /// additional rights declared by a source block.
    pub required_authorities: Vec<BackendAuthority>,
}
/// How an OValue is rendered into a backend's splice buffer. The actual
/// renderer functions live in `eval_core.rs` (they need OValue); the registry
/// only records which strategy a backend uses, so dispatch stays centralized
/// while the value-level code stays with the evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpliceRenderer {
    /// Python literals (`None`, `True`, `[1, 2]`, …).
    Python,
    /// Embeddable HTML markup (blobs become data-URI `<img>` tags).
    Html,
    /// LaTeX-safe text.
    Latex,
    /// Markdown-safe text.
    Markdown,
    /// Nix-oriented source expressions. Captured raw `NixExpr` bodies retain
    /// their own backend parse obligation; other values render inertly.
    Nix,
    /// `OValue::splice_repr()` — the conservative cross-language form.
    Default,
}

impl SpliceRenderer {
    const fn label(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Html => "html",
            Self::Latex => "latex",
            Self::Markdown => "markdown",
            Self::Nix => "nix",
            Self::Default => "default",
        }
    }
}

/// The concrete implementation boundary used after an operation has selected
/// its high-level [`ExecutionMode`]. `Shim` means framed hosted execution; it
/// does not by itself say whether the current Rust executable implements that
/// backend or proxies a compatibility Python shim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendAdapterKind {
    /// Implemented entirely inside the evaluator; no backend process is used.
    Inline,
    /// Implemented by `backend.rs` inside this runtime engine.
    NativeRust,
    /// Implemented by a Python compatibility shim reached through the Rust
    /// backend proxy.
    LegacyPythonShim,
}

impl BackendAdapterKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::NativeRust => "native-rust",
            Self::LegacyPythonShim => "legacy-python-shim",
        }
    }
}

/// How precisely a backend-wide executable requirement describes every source
/// body for that backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeRequirementPrecision {
    /// Exact for ordinary OIR dispatch through the backend's declared
    /// `ExecutionMode`. Auxiliary/direct wire endpoints are outside this
    /// backend-wide discovery projection.
    Exact,
    /// A safe backend-wide over-approximation. Operation-specific analysis may
    /// later prove that a subset of these commands is sufficient.
    ConservativeAllSources,
}

impl RuntimeRequirementPrecision {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::ConservativeAllSources => "conservative-all-sources",
        }
    }
}

/// One reusable executable requirement group. Alternatives are an ordered OR
/// of ordered AND command sets: `[["dotnet"], ["mcs", "mono"]]` means
/// `dotnet` OR (`mcs` AND `mono`). This is descriptive availability metadata;
/// it does not grant authority or establish runtime health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeRequirementSpec {
    pub key: &'static str,
    pub builtin: bool,
    pub precision: RuntimeRequirementPrecision,
    pub alternatives: &'static [&'static [&'static str]],
}

const UNKNOWN_RUNTIME_REQUIREMENT: RuntimeRequirementSpec = RuntimeRequirementSpec {
    key: "unknown-legacy-python-shim",
    builtin: false,
    precision: RuntimeRequirementPrecision::ConservativeAllSources,
    alternatives: &[&["python3"]],
};

/// Named bounded crossing profile attached to a canonical backend catalog row.
///
/// Catalog V5 binds only this profile label. The profile remains descriptive
/// shadow metadata: it does not extend [`BackendInterface`], grant execution
/// authority, or make a claim about every structural edge through a backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// Static metadata for one backend: the single source of truth for aliases,
/// purity, rendering, execution mode, authority, adapter ownership, and
/// executable requirements.
#[derive(Debug, Clone)]
pub struct BackendSpec {
    /// Canonical backend name as it appears in a language tag.
    pub name: &'static str,
    /// Alternate tag spellings accepted by splice rendering (`py`, `md`, …).
    pub aliases: &'static [&'static str],
    /// Whether `{lazy}` may cache results from this backend.
    ///
    /// This is a cache-safety contract for the current invocation mode, not
    /// a claim that the source language is mathematically pure. `pure: true`
    /// means: "safe for generic `{lazy}` memoization under the current
    /// fingerprint (body + dep identities + env id) and runtime model" —
    /// no hidden IO, clocks, randomness, or mutable external state can leak
    /// into the result. Shim-backed backends that run arbitrary programs in
    /// an unrestricted host environment must be `false` even when the
    /// backend language is nominally pure or declarative. `{defer}` works on
    /// any backend (it never caches), so it's the impure-backend escape
    /// hatch.
    pub pure: bool,
    /// Which splice-rendering strategy `render_child` should use.
    pub renderer: SpliceRenderer,
    /// How the evaluator dispatches this backend.
    pub execution: ExecutionMode,
    /// Rights needed to implement this backend. For example,
    /// the Bash adapter must start `bash`, while Python evaluation itself does
    /// not require a child process.
    pub required_authorities: &'static [BackendAuthority],
    /// Concrete adapter ownership. This refines `execution` without changing
    /// the OIR-level execution contract.
    pub adapter: BackendAdapterKind,
    /// Key into the canonical runtime-requirement catalog.
    pub runtime_requirement_key: &'static str,
    /// Representation facts used by fidelity and placement analysis.
    pub value_capabilities: BackendValueCapabilities,
    /// Explicit state behavior for this exact catalogued implementation.
    pub state_support: BackendStateSupportV2,
}

impl BackendSpec {
    fn matches(&self, lang: &str) -> bool {
        self.name == lang || self.aliases.contains(&lang)
    }
}

macro_rules! runtime_requirement_catalog {
    (
        $(
            {
                key: $key:literal,
                builtin: $builtin:literal,
                precision: $precision:ident,
                alternatives: [$([$($command:literal),* $(,)?]),* $(,)?],
            }
        ),* $(,)?
    ) => {
        const RUNTIME_REQUIREMENT_SPECS: &[RuntimeRequirementSpec] = &[
            $(
                RuntimeRequirementSpec {
                    key: $key,
                    builtin: $builtin,
                    precision: RuntimeRequirementPrecision::$precision,
                    alternatives: &[$(&[$($command),*]),*],
                },
            )*
        ];
    };
}

macro_rules! backend_catalog_metadata {
    (
        current_schema: $current_schema:literal,
        legacy_schema_v5: $legacy_schema_v5:literal,
        legacy_schema_v4: $legacy_schema_v4:literal,
        legacy_schema_v3: $legacy_schema_v3:literal $(,)?
    ) => {
        /// Legacy catalog domain retained for archival V3 inspection.
        pub const BACKEND_CATALOG_SCHEMA_V3: &str = $legacy_schema_v3;
        /// Archival V4 catalog domain retained byte-for-byte.
        pub const BACKEND_CATALOG_SCHEMA_V4: &str = $legacy_schema_v4;
        /// Archival V5 catalog domain retained byte-for-byte.
        pub const BACKEND_CATALOG_SCHEMA_V5: &str = $legacy_schema_v5;
        /// Current catalog domain. Only identities derived under this domain
        /// authorize new placement records.
        pub const BACKEND_CATALOG_SCHEMA_V6: &str = $current_schema;
        pub const BACKEND_CATALOG_CURRENT_SCHEMA: &str = BACKEND_CATALOG_SCHEMA_V6;
        /// Compatibility name retained for evidence code that predates the
        /// explicit current-schema constant. It always names the current domain.
        pub const BACKEND_CATALOG_SCHEMA_V1: &str = BACKEND_CATALOG_CURRENT_SCHEMA;
    };
}

macro_rules! integer_exactness {
    (Unknown) => {
        IntegerExactness::Unknown
    };
    (ExactMagnitudeBits($bits:literal)) => {
        IntegerExactness::ExactMagnitudeBits($bits)
    };
    (TwosComplementBits($bits:literal)) => {
        IntegerExactness::TwosComplementBits($bits)
    };
    (ExactRange { min: $min:literal, max: $max:literal }) => {{
        let min = BigInt::parse_bytes($min.as_bytes(), 10)
            .expect("backend catalog exact-range minimum is not a signed base-10 integer");
        let max = BigInt::parse_bytes($max.as_bytes(), 10)
            .expect("backend catalog exact-range maximum is not a signed base-10 integer");
        assert_eq!(
            min.to_str_radix(10),
            $min,
            "backend catalog exact-range minimum must use canonical signed base-10 spelling"
        );
        assert_eq!(
            max.to_str_radix(10),
            $max,
            "backend catalog exact-range maximum must use canonical signed base-10 spelling"
        );
        assert!(
            min <= max,
            "backend catalog exact-range minimum exceeds maximum"
        );
        IntegerExactness::ExactRange { min, max }
    }};
    (Arbitrary) => {
        IntegerExactness::Arbitrary
    };
}

macro_rules! state_support {
    (Stateless) => {
        BackendStateSupportV2::Stateless
    };
    (SemanticSnapshot { codec: $codec:literal, compatibility: ExactImplementation }) => {
        BackendStateSupportV2::SemanticSnapshot {
            codec: SemanticDigestV1::hash_bytes(
                "ostadix/backend-state-codec-name/v2",
                $codec.as_bytes(),
            ),
            compatibility: SnapshotCompatibilityV2::ExactImplementation,
        }
    };
    (
        SemanticSnapshot {
            codec: $codec:literal,
            compatibility: CompatibilityClass($class:literal)
        }
    ) => {
        BackendStateSupportV2::SemanticSnapshot {
            codec: SemanticDigestV1::hash_bytes(
                "ostadix/backend-state-codec-name/v2",
                $codec.as_bytes(),
            ),
            compatibility: SnapshotCompatibilityV2::CompatibilityClass(
                SemanticDigestV1::hash_bytes(
                    "ostadix/backend-state-compatibility-class-name/v2",
                    $class.as_bytes(),
                ),
            ),
        }
    };
    (ExternalPinned { manifest_schema: $manifest_schema:literal }) => {
        BackendStateSupportV2::ExternalPinned {
            manifest_schema: SemanticDigestV1::hash_bytes(
                "ostadix/external-state-manifest-schema-name/v2",
                $manifest_schema.as_bytes(),
            ),
        }
    };
}

macro_rules! morphism_profile {
    (None) => {
        None
    };
    (PythonPlainData) => {
        Some(BackendMorphismProfileV1::PythonPlainData)
    };
    (JavascriptBindingStdout) => {
        Some(BackendMorphismProfileV1::JavascriptBindingStdout)
    };
    (RustSourceConstantStdout) => {
        Some(BackendMorphismProfileV1::RustSourceConstantStdout)
    };
}

macro_rules! backend_catalog {
    (
        $(
            {
                name: $name:literal,
                aliases: [$($alias:literal),* $(,)?],
                pure: $pure:literal,
                renderer: $renderer:ident,
                execution: $execution:ident,
                authorities: [$($authority:ident),* $(,)?],
                adapter: $adapter:ident,
                runtime: $runtime:literal,
                integer_exactness: $integer_exactness:ident
                    $(($($integer_arguments:literal),* $(,)?))?
                    $({ min: $integer_min:literal, max: $integer_max:literal })?,
                rich_numbers: $rich_numbers:ident,
                state_support: $state_support:ident
                    $({
                        $($state_key:ident: $state_value:tt),* $(,)?
                    })?,
                morphism_profile: $morphism_profile:ident,
            }
        ),* $(,)?
    ) => {
        static BACKEND_SPECS: LazyLock<Vec<BackendSpec>> = LazyLock::new(|| vec![
            $(
                BackendSpec {
                    name: $name,
                    aliases: &[$($alias),*],
                    pure: $pure,
                    renderer: SpliceRenderer::$renderer,
                    execution: ExecutionMode::$execution,
                    required_authorities: &[$(BackendAuthority::$authority),*],
                    adapter: BackendAdapterKind::$adapter,
                    runtime_requirement_key: $runtime,
                    value_capabilities: BackendValueCapabilities {
                        integer_exactness: integer_exactness!(
                            $integer_exactness
                            $(($($integer_arguments),*))?
                            $({ min: $integer_min, max: $integer_max })?
                        ),
                        rich_numbers: RichNumberPreservation::$rich_numbers,
                    },
                    state_support: state_support!(
                        $state_support
                        $({ $($state_key: $state_value),* })?
                    ),
                },
            )*
        ]);
        const BACKEND_MORPHISM_PROFILE_ASSIGNMENTS: &[
            (&str, Option<BackendMorphismProfileV1>)
        ] = &[
            $(
                ($name, morphism_profile!($morphism_profile)),
            )*
        ];
    };
}

// The included file is pure declarative data and is also embedded verbatim by
// olangc so emitted runtime projects compile from the identical catalog.
include!("backend_catalog.inc.rs");

// Catalog V3 through V5 published the original two WebAssembly alternatives.
// The current declarative catalog may grow, but those archival identities must
// continue hashing the exact runtime requirement they originally named.
const ARCHIVAL_WEBASSEMBLY_ALTERNATIVES_V5: &[&[&str]] =
    &[&["wat2wasm", "wasmtime"], &["wat2wasm", "wasmer"]];
const ARCHIVAL_WEBASSEMBLY_REQUIREMENT_V5: RuntimeRequirementSpec = RuntimeRequirementSpec {
    key: "webassembly",
    builtin: false,
    precision: RuntimeRequirementPrecision::ConservativeAllSources,
    alternatives: ARCHIVAL_WEBASSEMBLY_ALTERNATIVES_V5,
};

fn archival_runtime_requirement_v5(
    requirement: &'static RuntimeRequirementSpec,
) -> &'static RuntimeRequirementSpec {
    if requirement.key == "webassembly" {
        &ARCHIVAL_WEBASSEMBLY_REQUIREMENT_V5
    } else {
        requirement
    }
}

pub(crate) fn catalog_hash_field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

fn catalog_hash_count(hash: &mut Sha256, count: usize) {
    hash.update((count as u64).to_be_bytes());
}

fn hash_runtime_requirement(hash: &mut Sha256, requirement: &RuntimeRequirementSpec) {
    catalog_hash_field(hash, requirement.key.as_bytes());
    catalog_hash_field(
        hash,
        if requirement.builtin {
            b"builtin"
        } else {
            b"external"
        },
    );
    catalog_hash_field(hash, requirement.precision.name().as_bytes());
    catalog_hash_count(hash, requirement.alternatives.len());
    for alternative in requirement.alternatives {
        catalog_hash_count(hash, alternative.len());
        for command in *alternative {
            catalog_hash_field(hash, command.as_bytes());
        }
    }
}

pub(crate) fn hash_backend_spec_v3(
    hash: &mut Sha256,
    spec: &BackendSpec,
    requirement: &RuntimeRequirementSpec,
) {
    catalog_hash_field(hash, spec.name.as_bytes());
    catalog_hash_count(hash, spec.aliases.len());
    for alias in spec.aliases {
        catalog_hash_field(hash, alias.as_bytes());
    }
    catalog_hash_field(hash, if spec.pure { b"pure" } else { b"impure" });
    catalog_hash_field(hash, spec.renderer.label().as_bytes());
    catalog_hash_field(hash, spec.execution.label().as_bytes());
    catalog_hash_count(hash, spec.required_authorities.len());
    for authority in spec.required_authorities {
        catalog_hash_field(hash, authority.name().as_bytes());
    }
    catalog_hash_field(hash, spec.adapter.name().as_bytes());
    catalog_hash_field(hash, spec.runtime_requirement_key.as_bytes());
    let integer_exactness = &spec.value_capabilities.integer_exactness;
    catalog_hash_field(hash, integer_exactness.label().as_bytes());
    match integer_exactness {
        IntegerExactness::ExactMagnitudeBits(bits) | IntegerExactness::TwosComplementBits(bits) => {
            catalog_hash_field(hash, &bits.to_be_bytes());
        }
        IntegerExactness::ExactRange { min, max } => {
            catalog_hash_field(hash, min.to_str_radix(10).as_bytes());
            catalog_hash_field(hash, max.to_str_radix(10).as_bytes());
        }
        IntegerExactness::Unknown | IntegerExactness::Arbitrary => {}
    }
    catalog_hash_field(
        hash,
        spec.value_capabilities.rich_numbers.label().as_bytes(),
    );
    hash_runtime_requirement(hash, requirement);
}

fn hash_state_support(hash: &mut Sha256, support: &BackendStateSupportV2) {
    match support {
        BackendStateSupportV2::Stateless => {
            catalog_hash_field(hash, b"stateless");
        }
        BackendStateSupportV2::SemanticSnapshot {
            codec,
            compatibility,
        } => {
            catalog_hash_field(hash, b"semantic-snapshot");
            catalog_hash_field(hash, codec.as_sha256().as_bytes());
            match compatibility {
                SnapshotCompatibilityV2::ExactImplementation => {
                    catalog_hash_field(hash, b"exact-implementation");
                }
                SnapshotCompatibilityV2::CompatibilityClass(class) => {
                    catalog_hash_field(hash, b"compatibility-class");
                    catalog_hash_field(hash, class.as_sha256().as_bytes());
                }
            }
        }
        BackendStateSupportV2::ExternalPinned { manifest_schema } => {
            catalog_hash_field(hash, b"external-pinned");
            catalog_hash_field(hash, manifest_schema.as_sha256().as_bytes());
        }
    }
}

pub(crate) fn hash_backend_spec_v4(
    hash: &mut Sha256,
    spec: &BackendSpec,
    requirement: &RuntimeRequirementSpec,
) {
    // Archival V4 is the exact V3 projection extended by one explicit
    // state-support field. Keeping the shared prefix makes the rollover
    // auditable while the distinct schema domain prevents cross-version
    // authorization. Its field order and encoding are compatibility-frozen.
    hash_backend_spec_v3(hash, spec, requirement);
    hash_state_support(hash, &spec.state_support);
}

pub(crate) fn hash_backend_spec_v5(
    hash: &mut Sha256,
    spec: &BackendSpec,
    requirement: &RuntimeRequirementSpec,
    morphism_profile: Option<BackendMorphismProfileV1>,
) {
    // V5 is the exact archival V4 projection extended by one explicitly
    // discriminated optional profile. Length-framed fields make absence,
    // presence, and every profile name unambiguous.
    hash_backend_spec_v4(hash, spec, requirement);
    match morphism_profile {
        None => catalog_hash_field(hash, b"no-backend-morphism-profile"),
        Some(profile) => {
            catalog_hash_field(hash, b"backend-morphism-profile");
            catalog_hash_field(hash, profile.name().as_bytes());
        }
    }
}

pub(crate) fn hash_backend_spec_v6(
    hash: &mut Sha256,
    spec: &BackendSpec,
    requirement: &RuntimeRequirementSpec,
    morphism_profile: Option<BackendMorphismProfileV1>,
) {
    // V6 retains the complete V5 projection shape. Its distinct schema domain
    // authorizes the expanded runtime alternatives without rewriting V5.
    hash_backend_spec_v5(hash, spec, requirement, morphism_profile);
}

pub(crate) fn finish_catalog_hash(hash: Sha256) -> String {
    hex::encode(hash.finalize())
}

/// Lookup table over `BackendSpec`s plus the centralized shim path
/// resolution rule. Today the table is static; `BackendRegistry` is the
/// place where dynamically registered backends would plug in later.
#[derive(Debug)]
pub struct BackendRegistry {
    specs: &'static [BackendSpec],
}

impl SyntaxDialect for std::collections::HashSet<String> {
    fn is_registered_syntax_tag(&self, name: &str) -> bool {
        self.contains(name)
    }

    fn canonical_syntax_name(&self, name: &str) -> String {
        BackendRegistry::global().canonical(name).to_owned()
    }

    fn owns_quoted_syntax(&self, canonical_name: &str) -> bool {
        let backend = BackendRegistry::global().interface_for(canonical_name);
        backend.execution == ExecutionMode::InlineAst && backend.canonical == "quote"
    }
}

impl BackendRegistry {
    /// Fallback metadata for backends with no entry in the table:
    /// impure, conservative cross-language splice representation.
    const DEFAULT_SPEC: BackendSpec = BackendSpec {
        name: "",
        aliases: &[],
        pure: false,
        renderer: SpliceRenderer::Default,
        execution: ExecutionMode::Shim,
        required_authorities: &[
            BackendAuthority::FileRead,
            BackendAuthority::FileWrite,
            BackendAuthority::Network,
            BackendAuthority::Process,
        ],
        adapter: BackendAdapterKind::LegacyPythonShim,
        runtime_requirement_key: "unknown-legacy-python-shim",
        value_capabilities: BackendValueCapabilities::UNKNOWN,
        state_support: BackendStateSupportV2::Stateless,
    };

    /// The process-wide registry over the static spec table.
    pub fn global() -> &'static BackendRegistry {
        static REGISTRY: OnceLock<BackendRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| BackendRegistry {
            specs: BACKEND_SPECS.as_slice(),
        })
    }

    /// Look up a backend by canonical name or alias.
    pub fn get(&self, lang: &str) -> Option<&BackendSpec> {
        self.specs.iter().find(|s| s.matches(lang))
    }

    /// Canonical backend specifications in their stable catalog order.
    pub fn canonical_specs(&self) -> &'static [BackendSpec] {
        self.specs
    }

    /// Reusable executable requirement groups in their stable discovery order.
    pub fn runtime_requirement_specs(&self) -> &'static [RuntimeRequirementSpec] {
        RUNTIME_REQUIREMENT_SPECS
    }

    /// Resolve descriptive executable requirements for a canonical name or
    /// alias. Unknown tags retain the conservative legacy-Python fallback.
    pub fn runtime_requirements_for(&self, lang: &str) -> &'static RuntimeRequirementSpec {
        let key = self
            .get(lang)
            .map_or(Self::DEFAULT_SPEC.runtime_requirement_key, |spec| {
                spec.runtime_requirement_key
            });
        RUNTIME_REQUIREMENT_SPECS
            .iter()
            .find(|requirement| requirement.key == key)
            .unwrap_or(&UNKNOWN_RUNTIME_REQUIREMENT)
    }

    fn runtime_requirements_for_v5(&self, lang: &str) -> &'static RuntimeRequirementSpec {
        archival_runtime_requirement_v5(self.runtime_requirements_for(lang))
    }

    /// Concrete adapter ownership for a canonical name or alias. Unknown tags
    /// remain conservative compatibility-shim backends.
    pub fn adapter_for(&self, lang: &str) -> BackendAdapterKind {
        self.get(lang)
            .map_or(Self::DEFAULT_SPEC.adapter, |spec| spec.adapter)
    }

    /// Value-representation facts for a canonical backend or alias. Unknown
    /// compatibility backends return an explicit all-unknown descriptor.
    pub fn value_capabilities_for(&self, lang: &str) -> BackendValueCapabilities {
        self.get(lang).map_or_else(
            || BackendValueCapabilities::UNKNOWN,
            |spec| spec.value_capabilities.clone(),
        )
    }

    /// State behavior for a canonical current entry or alias. Unknown tags
    /// have no state authority and therefore return `None`.
    pub fn state_support_for(&self, lang: &str) -> Option<&BackendStateSupportV2> {
        self.get(lang).map(|spec| &spec.state_support)
    }

    /// Bounded shadow crossing profile for a canonical name or alias. Unknown
    /// and explicitly unprofiled backends both return None.
    pub fn morphism_profile_for(&self, lang: &str) -> Option<BackendMorphismProfileV1> {
        let canonical = self.get(lang)?.name;
        BACKEND_MORPHISM_PROFILE_ASSIGNMENTS
            .iter()
            .find(|(name, _)| *name == canonical)
            .and_then(|(_, profile)| *profile)
    }

    /// Deterministic SHA-256 of the complete ordered canonical catalog. This
    /// identifies descriptive metadata only; it is not runtime readiness or
    /// execution authority.
    pub fn catalog_sha256_v3(&self) -> String {
        static V3_DIGEST: OnceLock<String> = OnceLock::new();
        V3_DIGEST
            .get_or_init(|| {
                let mut hash = Sha256::new();
                catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V3.as_bytes());
                catalog_hash_count(&mut hash, RUNTIME_REQUIREMENT_SPECS.len());
                for requirement in RUNTIME_REQUIREMENT_SPECS {
                    hash_runtime_requirement(
                        &mut hash,
                        archival_runtime_requirement_v5(requirement),
                    );
                }
                catalog_hash_count(&mut hash, self.specs.len());
                for spec in self.specs {
                    let requirement = self.runtime_requirements_for_v5(spec.name);
                    hash_backend_spec_v3(&mut hash, spec, requirement);
                }
                finish_catalog_hash(hash)
            })
            .clone()
    }

    /// Deterministic SHA-256 of the complete ordered archival V4 catalog.
    pub fn catalog_sha256_v4(&self) -> String {
        static V4_DIGEST: OnceLock<String> = OnceLock::new();
        V4_DIGEST
            .get_or_init(|| {
                let mut hash = Sha256::new();
                catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V4.as_bytes());
                catalog_hash_count(&mut hash, RUNTIME_REQUIREMENT_SPECS.len());
                for requirement in RUNTIME_REQUIREMENT_SPECS {
                    hash_runtime_requirement(
                        &mut hash,
                        archival_runtime_requirement_v5(requirement),
                    );
                }
                catalog_hash_count(&mut hash, self.specs.len());
                for spec in self.specs {
                    hash_backend_spec_v4(
                        &mut hash,
                        spec,
                        self.runtime_requirements_for_v5(spec.name),
                    );
                }
                finish_catalog_hash(hash)
            })
            .clone()
    }

    /// Deterministic SHA-256 of the complete ordered archival V5 catalog.
    pub fn catalog_sha256_v5(&self) -> String {
        static V5_DIGEST: OnceLock<String> = OnceLock::new();
        V5_DIGEST
            .get_or_init(|| {
                let mut hash = Sha256::new();
                catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V5.as_bytes());
                catalog_hash_count(&mut hash, RUNTIME_REQUIREMENT_SPECS.len());
                for requirement in RUNTIME_REQUIREMENT_SPECS {
                    hash_runtime_requirement(
                        &mut hash,
                        archival_runtime_requirement_v5(requirement),
                    );
                }
                catalog_hash_count(&mut hash, self.specs.len());
                for spec in self.specs {
                    hash_backend_spec_v5(
                        &mut hash,
                        spec,
                        self.runtime_requirements_for_v5(spec.name),
                        self.morphism_profile_for(spec.name),
                    );
                }
                finish_catalog_hash(hash)
            })
            .clone()
    }

    /// Deterministic SHA-256 of the complete ordered current V6 catalog.
    pub fn catalog_sha256_v6(&self) -> String {
        static V6_DIGEST: OnceLock<String> = OnceLock::new();
        V6_DIGEST
            .get_or_init(|| {
                let mut hash = Sha256::new();
                catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V6.as_bytes());
                catalog_hash_count(&mut hash, RUNTIME_REQUIREMENT_SPECS.len());
                for requirement in RUNTIME_REQUIREMENT_SPECS {
                    hash_runtime_requirement(&mut hash, requirement);
                }
                catalog_hash_count(&mut hash, self.specs.len());
                for spec in self.specs {
                    hash_backend_spec_v6(
                        &mut hash,
                        spec,
                        self.runtime_requirements_for(spec.name),
                        self.morphism_profile_for(spec.name),
                    );
                }
                finish_catalog_hash(hash)
            })
            .clone()
    }

    /// Deterministic SHA-256 of the complete current ordered catalog.
    /// Current behavior is an alias of the explicit V6 helper.
    pub fn catalog_sha256(&self) -> String {
        self.catalog_sha256_v6()
    }

    /// Deterministic SHA-256 of one canonical backend specification and its
    /// referenced runtime requirements. Aliases resolve to the same digest.
    pub fn specification_sha256_v3(&self, lang: &str) -> Option<String> {
        let spec = self.get(lang)?;
        let mut hash = Sha256::new();
        catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V3.as_bytes());
        hash_backend_spec_v3(&mut hash, spec, self.runtime_requirements_for_v5(spec.name));
        Some(finish_catalog_hash(hash))
    }

    /// Deterministic SHA-256 of one archival V4 canonical backend
    /// specification. Aliases resolve to the same exact identity.
    pub fn specification_sha256_v4(&self, lang: &str) -> Option<String> {
        let spec = self.get(lang)?;
        let mut hash = Sha256::new();
        catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V4.as_bytes());
        hash_backend_spec_v4(&mut hash, spec, self.runtime_requirements_for_v5(spec.name));
        Some(finish_catalog_hash(hash))
    }

    /// Deterministic SHA-256 of one archival V5 canonical backend
    /// specification. Aliases resolve to the same exact identity.
    pub fn specification_sha256_v5(&self, lang: &str) -> Option<String> {
        let spec = self.get(lang)?;
        let mut hash = Sha256::new();
        catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V5.as_bytes());
        hash_backend_spec_v5(
            &mut hash,
            spec,
            self.runtime_requirements_for_v5(spec.name),
            self.morphism_profile_for(spec.name),
        );
        Some(finish_catalog_hash(hash))
    }

    /// Deterministic SHA-256 of one current V6 canonical backend
    /// specification. Aliases resolve to the same exact identity.
    pub fn specification_sha256_v6(&self, lang: &str) -> Option<String> {
        let spec = self.get(lang)?;
        let mut hash = Sha256::new();
        catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V6.as_bytes());
        hash_backend_spec_v6(
            &mut hash,
            spec,
            self.runtime_requirements_for(spec.name),
            self.morphism_profile_for(spec.name),
        );
        Some(finish_catalog_hash(hash))
    }

    /// Deterministic SHA-256 of one current canonical backend specification.
    /// Current behavior is an alias of the explicit V6 helper.
    pub fn specification_sha256(&self, lang: &str) -> Option<String> {
        self.specification_sha256_v6(lang)
    }

    /// Build the exact implementation identity shared by local publication
    /// and evaluator placement preflight.
    ///
    /// `backend` may be a canonical name or a declared alias; both resolve
    /// through the current catalog and therefore produce identical identities.
    /// When an admitted caller already carries a backend-specification digest,
    /// it supplies that value through `expected_backend_specification`; a
    /// stale or foreign catalog coordinate then fails closed before an
    /// implementation identity can be minted. `None` is reserved for callers
    /// such as local discovery that intentionally derive the coordinate from
    /// the process's current catalog.
    ///
    /// This function is pure: artifact and executable-set discovery remains
    /// with the caller, while this method owns the canonical realization
    /// formula and its catalog/adapter interpretation.
    pub fn backend_implementation_id_v1(
        &self,
        backend: &str,
        expected_backend_specification: Option<&SemanticDigestV1>,
        adapter_artifact: ArtifactId,
        executable_set: SemanticDigestV1,
        protocol_abi: impl Into<String>,
    ) -> Result<BackendImplementationIdV1, PlacementValidationError> {
        let spec = self
            .get(backend)
            .ok_or_else(|| PlacementValidationError::InvalidToken {
                field: "backend implementation canonical backend",
                value: backend.to_owned(),
            })?;
        let backend_specification =
            SemanticDigestV1::from_sha256(self.specification_sha256(spec.name).ok_or_else(
                || PlacementValidationError::InvalidToken {
                    field: "backend implementation canonical backend",
                    value: backend.to_owned(),
                },
            )?)?;
        if let Some(expected) = expected_backend_specification {
            if expected != &backend_specification {
                if !self.contains_specification_sha256(expected.as_sha256()) {
                    return Err(PlacementValidationError::NonCurrentBackendCatalog {
                        specification: expected.as_sha256().to_owned(),
                        current_schema: BACKEND_CATALOG_CURRENT_SCHEMA.to_owned(),
                    });
                }
                return Err(PlacementValidationError::ScopeMismatch {
                    field: "backend implementation specification",
                    expected: backend_specification.as_sha256().to_owned(),
                    got: expected.as_sha256().to_owned(),
                });
            }
        }

        let protocol_abi = protocol_abi.into();
        if protocol_abi != LOCAL_BACKEND_PROTOCOL_ABI_V1 {
            return Err(PlacementValidationError::ScopeMismatch {
                field: "backend implementation protocol ABI",
                expected: LOCAL_BACKEND_PROTOCOL_ABI_V1.to_owned(),
                got: protocol_abi,
            });
        }
        let realization_material = serde_json::json!({
            "schema": LOCAL_REALIZATION_SCHEMA_V2,
            "backend_specification": backend_specification.as_sha256(),
            "adapter_kind": spec.adapter.name(),
            "adapter_artifact": adapter_artifact.as_sha256(),
            "executable_set_schema": BACKEND_EXECUTABLE_SET_DIGEST_DOMAIN_V2,
            "executable_set": executable_set.as_sha256(),
            "protocol": protocol_abi.as_str(),
        });
        let realization_bytes = serde_json::to_vec(&realization_material)
            .map_err(|error| PlacementValidationError::CanonicalSerialization(error.to_string()))?;
        let realization_pipeline =
            SemanticDigestV1::hash_bytes(LOCAL_REALIZATION_DIGEST_DOMAIN_V2, &realization_bytes);
        BackendImplementationIdV1::new(
            backend_specification,
            adapter_artifact,
            executable_set,
            protocol_abi,
            realization_pipeline,
        )
    }

    /// Whether `digest` is the specification identity of a canonical backend
    /// under the current catalog schema and hash domain.
    ///
    /// This is deliberately stricter than accepting a well-formed SHA-256:
    /// legacy catalog-domain digests and arbitrary unknown digests are not
    /// current implementation identities, even when their old records remain
    /// structurally inspectable.
    pub fn contains_specification_sha256(&self, digest: &str) -> bool {
        self.specs.iter().any(|spec| {
            self.specification_sha256(spec.name)
                .is_some_and(|current| current == digest)
        })
    }

    /// Resolve a language tag (canonical name or alias) to its canonical
    /// name. Unknown tags are returned unchanged.
    pub fn canonical<'a>(&self, lang: &'a str) -> &'a str {
        self.get(lang).map_or(lang, |s| s.name)
    }

    /// Whether `{lazy}` may cache results from this backend.
    /// Unknown backends are conservatively impure.
    pub fn is_pure(&self, lang: &str) -> bool {
        self.get(lang).is_some_and(|s| s.pure)
    }

    /// Which splice-rendering strategy `render_child` should use for `lang`.
    /// Unknown backends use the conservative default representation.
    pub fn renderer_for(&self, lang: &str) -> SpliceRenderer {
        self.get(lang)
            .map_or(Self::DEFAULT_SPEC.renderer, |s| s.renderer)
    }

    /// Typed backend interface metadata used by planning and dispatch policy.
    pub fn interface_for(&self, lang: &str) -> BackendInterface {
        let canonical = self.canonical(lang).to_string();
        let fallback = Self::DEFAULT_SPEC;
        let spec = self.get(lang).unwrap_or(&fallback);
        BackendInterface {
            canonical,
            specification_sha256: self.specification_sha256(lang),
            pure: spec.pure,
            renderer: spec.renderer,
            execution: spec.execution,
            value_capabilities: spec.value_capabilities.clone(),
            state_support: self.state_support_for(lang).cloned(),
            required_authorities: spec.required_authorities.to_vec(),
        }
    }

    /// Centralized shim path resolution.
    ///
    /// Probes, in order: `<dir>/<lang>_shim.py`, `<dir>/<lang>_shim`,
    /// `<dir>/<lang>.py`, `<dir>/<lang>`. If none exists on disk, falls back
    /// to `<dir>/<lang>_shim.py` so the eventual spawn error names the
    /// conventional path.
    pub fn resolve_shim_path(&self, shim_dir: &Path, lang: &str) -> PathBuf {
        let candidates = [
            shim_dir.join(format!("{lang}_shim.py")),
            shim_dir.join(format!("{lang}_shim")),
            shim_dir.join(format!("{lang}.py")),
            shim_dir.join(lang),
        ];
        candidates
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| shim_dir.join(format!("{lang}_shim.py")))
    }

    /// All parser tags accepted by the registry: every canonical backend
    /// name plus every declared alias, in the deterministic order of the
    /// static spec table (canonical name first, then its aliases).
    ///
    /// This is the single source of truth for the set of accepted language
    /// tags; binaries must not maintain their own copies.
    pub fn registered_backend_names(&self) -> Vec<&'static str> {
        self.specs
            .iter()
            .flat_map(|s| std::iter::once(s.name).chain(s.aliases.iter().copied()))
            .collect()
    }

    /// Convenience: the accepted tag set as owned `String`s, ready for
    /// `Parser::new` / `Evaluator::with_registered_backends`.
    pub fn registered_backend_tags(&self) -> std::collections::HashSet<String> {
        self.registered_backend_names()
            .into_iter()
            .map(str::to_string)
            .collect()
    }
}

impl CurrentBackendCatalogV1 for BackendRegistry {
    fn current_schema(&self) -> &str {
        BACKEND_CATALOG_CURRENT_SCHEMA
    }

    fn contains_current_specification(&self, digest: &SemanticDigestV1) -> bool {
        self.contains_specification_sha256(digest.as_sha256())
    }

    fn contains_current_implementation(&self, implementation: &BackendImplementationIdV1) -> bool {
        let Some(spec) = self.specs.iter().find(|spec| {
            self.specification_sha256(spec.name).as_deref()
                == Some(implementation.backend_specification().as_sha256())
        }) else {
            return false;
        };
        self.backend_implementation_id_v1(
            spec.name,
            Some(implementation.backend_specification()),
            implementation.adapter_artifact().clone(),
            implementation.executable_set().clone(),
            implementation.protocol_abi(),
        )
        .is_ok_and(|current| current == *implementation)
    }

    fn state_support_for_current_specification(
        &self,
        digest: &SemanticDigestV1,
    ) -> Option<&BackendStateSupportV2> {
        self.specs.iter().find_map(|spec| {
            self.specification_sha256(spec.name)
                .is_some_and(|current| current == digest.as_sha256())
                .then_some(&spec.state_support)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOG_V4_WHOLE_SHA256: &str =
        "abbe7201cd985baaa9c8da81a09791830a2cceb5697b514fea942852c25e10ea";
    const CATALOG_V4_SOURCE_SHA256: &str =
        "6d4e19cdf737982a5cfaf7d802c716e08ef5fc1e031465379a8a20348e08687a";
    const CATALOG_V4_SOURCE_BYTES: usize = 14_568;
    const CATALOG_V5_WHOLE_SHA256: &str =
        "9c7b98d7404b98af97d188abb96917db0edf091b22d3ae904eb14c509a2d4a78";
    const CATALOG_V5_SOURCE_SHA256: &str =
        "c5fd5baf4b282230f7be9be65af0728f2dfa722b85876865b8e7608a995df1bf";
    const CATALOG_V5_SOURCE_BYTES: usize = 15_630;
    const CATALOG_V6_WHOLE_SHA256: &str =
        "df3730ddf88eaea0e2e6e8973841d26718224062274dfbbf2ff1a227b0feb103";
    const CATALOG_V6_SOURCE_SHA256: &str =
        "6eddd348911053268d339c1e0888b7ab9bb8e4b05b7ecf6f98ec06fa432b37a7";
    const CATALOG_V6_SOURCE_BYTES: usize = 15_959;

    const CATALOG_V3_SPEC_GOLDENS: &[(&str, &str)] = &[
        (
            "O",
            "d950d5857e1dc57ea4f2a2ae4603c22809aa3fbb0ae72fb983c78c5a9e632594",
        ),
        (
            "quote",
            "a354062b89cdc800361a22973e7b22029ef3a8ba6c89ae517dd060323fe9584d",
        ),
        (
            "nix",
            "af2e5cb7ca31a435f11e4d963fd72f1602bdc34af22f952e0fa2ab79d5073d4e",
        ),
        (
            "nix_expr",
            "6df666a6848122bf99bd6a6fb0a69ca807dec1324decf3e056a94af43ac6fe5c",
        ),
        (
            "nix_store",
            "de30341e99237888ae48d1acaad65d008281dd6d596b27a9b7bac8472888dd66",
        ),
        (
            "nixos_test",
            "3aab03df2cd680d7525038bb7d8f996abceabf88f0fc1b538f0555469863956f",
        ),
        (
            "html",
            "09c84ed2860b7e489ac7b85019bea022776201f777fa46342ffab3f906eff9ab",
        ),
        (
            "markdown",
            "aee4c1a29734ebfbf378b20e4fc19d7fbfb8365fbcbdd5538d337e3239a0c427",
        ),
        (
            "latex",
            "06ed79952fca6fb4e5221a9f4f9f36a15f17aaef747fdf7bae2fa290d497a6ab",
        ),
        (
            "text",
            "636b9d9237b58af152c2fe92896306e491f8cde407d01a1c0857b3a465f8b551",
        ),
        (
            "sql",
            "db4b71ab62c1c528f63b4b3706e7bab8a0e8583800d8a9cce27763f52ae618ec",
        ),
        (
            "haskell",
            "22f9325f9ca3200747b05cdee3656ffd65159fd06bfb55093956247451852f16",
        ),
        (
            "ocaml",
            "37cd484f614d3f8c1d1c3d06d8f52f9ecfe7d1d07b7728f7362536f3b07cf9bb",
        ),
        (
            "webassembly",
            "51cdefb6d3d187f6bd3a3c2ee343c18c462048e9e025d0d8052a9cad2bf4d4aa",
        ),
        (
            "python",
            "dd078f6b0eb48e099cce81b39711fa62313d39c7f8915abd97d9e72bc7678ecc",
        ),
        (
            "ubuntu_vm",
            "e863e96d5b3ce3e2b57a0ec8ee0ceb06038aa19b7929f5852ff2f92e117f44d3",
        ),
        (
            "bash",
            "e89ea6eb57eea53b50ba1c3b7a83a64c79d36160af7bffb0b15f8d03560c10cd",
        ),
        (
            "shell",
            "31652f34b475bdc7956505e39d135af74f80659f0795ca54a0e8f8a76c116cf9",
        ),
        (
            "rust",
            "8744827a7d497396ab645f4abd6f2f3a660da2fb769449d1cfa76ba98b68c2e5",
        ),
        (
            "racket",
            "c4e53bf39f937d0c25282b6f4e6a1ecf7073a68e110910fe0193cb446f2393e5",
        ),
        (
            "csharp",
            "4a34c14b79e3631831c220f61d8a30151b77fd4ccd475f35147aa94ba6d00e5e",
        ),
        (
            "c",
            "d8842139f0f671a062f414069d5653dc880e23e3439e73323868829fbaf7210d",
        ),
        (
            "cpp",
            "9c33364ebeb787d05f0ec4f01cf2f429cdcb6adbed2c01c67595557660dc6b73",
        ),
        (
            "lisp",
            "48df5c9240a148db43ad011dbf92a669138111540338273557e7d86faa8132fd",
        ),
        (
            "common_lisp",
            "7da0053792edf03b618f61408429bdc93fa2d50387f731c4d365d61d1f3b03c7",
        ),
        (
            "ruby",
            "14891c157518c0c8f34622adc00a8c755447ab7a894386341548dbec21fd6513",
        ),
        (
            "matlab",
            "d3917e3c2cd83292ef23795ba0098ccc62112c827a62499f404200c58c4bb8cf",
        ),
        (
            "mathematica",
            "c67b1715316b2b22349046e9541792d9d62c81bf48a9dd08b0d9fd2559557760",
        ),
        (
            "java",
            "b97392e28423d3aa4fd47919f18ca97d056b18344df9cba952f2c8a6c83b5962",
        ),
        (
            "javascript",
            "f98d171a8e35e67baa2d2a0b7d586140a1e37ea2d580844d596bf80ce8dd9bac",
        ),
    ];

    const CATALOG_V4_SPEC_GOLDENS: &[(&str, &str)] = &[
        (
            "O",
            "b15701ebd9add8af496b4b04f26d7b2e004536d0872773753c17ce4b7ef0f906",
        ),
        (
            "quote",
            "8c1ce975a3f30c9fb15fb3ebf1634b72f741828dd7fe75e7239d0e34a45d9185",
        ),
        (
            "nix",
            "3f2aab4c05491d879ac539cbe525916c436724f41f36dd65914136c8319db1fc",
        ),
        (
            "nix_expr",
            "4eef9011432d05db6a7a3e193e2636c8dd84faa8164a2fa9ab178f5dd8b1a4e9",
        ),
        (
            "nix_store",
            "6cb9bcedba7abdc188d92d33a791fd8042823a454cb27f33be88eb3a133716d4",
        ),
        (
            "nixos_test",
            "684549354bf9f93ddf92685a61da05fb24c4e6b40a4c804637460336174a063a",
        ),
        (
            "html",
            "90fa3256cac531bb508ac63ff125a4404a68fe439c21b755c10c849da6292131",
        ),
        (
            "markdown",
            "01e3d22d55f365c4c7074b5b5ced2e04bfe15f96ee93e0f8758457d1c1e8c78b",
        ),
        (
            "latex",
            "1286ded0de2f25dc9aa0a04944bfd3339d4bc740659ac9949656c81566321096",
        ),
        (
            "text",
            "6b8fb8b5c63b74f22679dd2815ceb0fc32effb4179d673ac2e942b30647a4b88",
        ),
        (
            "sql",
            "ff2024b00eea4d2d259145dbb5ae37c62bcaabac107f2394d3a5f6c272b92455",
        ),
        (
            "haskell",
            "d59482a53b103667c3730b4ec93957795cb63794ddc4676f6a432f553defd7a1",
        ),
        (
            "ocaml",
            "39fee675d22a089ff4e48e572da39c933a67c5e39996109e90be4abacc943f2f",
        ),
        (
            "webassembly",
            "b536d61f9b533eb6e1cec3cbcbf5d025eab7b6bd67e0f700a4a5878aee6e3f7f",
        ),
        (
            "python",
            "0802014cbcadb8e7302ccf4f542d0b08eb5ccb05d41caffdac359e392145dcbf",
        ),
        (
            "ubuntu_vm",
            "663fd0a62109a2144ce17e8e469fbcb300870c5ec7ff3506eeb366e0717b290b",
        ),
        (
            "bash",
            "322a4c2cfca907216676baacd4d3ddc4f7a101276880950474aca8d74b6a62c4",
        ),
        (
            "shell",
            "96fb67c3f4d2b696602164817fa552df3f734177d72877f90d88d4db45a3765a",
        ),
        (
            "rust",
            "95d1538e220ed3ec9e38758f8ca70ae79dc385b7a2798adff2678e99668543d9",
        ),
        (
            "racket",
            "8b4b6c96e62928854445c425012ab4c96f2d3fc96d22e84d18497b13c33be702",
        ),
        (
            "csharp",
            "e44cc83afe013d5bb2ecf9c4d27fdf5377c37381221fdb6190f82e41d346f5a5",
        ),
        (
            "c",
            "8d06030127e282d4ef412c976cfb011e4c20370f0a069c07391bbf62e10611a6",
        ),
        (
            "cpp",
            "1d60c1ad941f05bb53098f277401f9d5a13ded43a99497d2179a6f426389e14c",
        ),
        (
            "lisp",
            "7b0cad859ced0b7d5e6a95a842162f063e534e7c420e81279bf526c055909f82",
        ),
        (
            "common_lisp",
            "cfddf29f81a4012b449a364058d7fb3c3bf5811b7d922f4349276ef3f5a401ea",
        ),
        (
            "ruby",
            "d0fe0b8f63f48633344a4fe0014cad15b44d88df5e1360250b88460c8dff8e38",
        ),
        (
            "matlab",
            "426064f002e0eb2b987c6cc81279a944b3f9c5411b32de618ae7c55a800600c4",
        ),
        (
            "mathematica",
            "2002761bd4d356b251cf995b220388de5af4f04d7f31948a9a1257517fd59c73",
        ),
        (
            "java",
            "2e99afd69b070eb73073c7ebc8b4d05e2e716345a1b7e28fe98cd4c4ee87b1f9",
        ),
        (
            "javascript",
            "2ef7f1459b3c11bb847239b1fa8fb0c950882b14d30aa5eefb64ad8dead111b6",
        ),
    ];

    const CATALOG_V5_SPEC_GOLDENS: &[(&str, &str)] = &[
        (
            "O",
            "4eaf4ee3f9378b04e4d2d83bbb975bb05903d007322fff4ec498cd98adf0350d",
        ),
        (
            "quote",
            "1c5a67e8f20c4bf2f3b01fc6cdb9fdd8f1950f6191571bd1b1fa4127c5ec23ec",
        ),
        (
            "nix",
            "84da0cee3957b1866ec03ca8b1aa6294a567547defa20dd26b232a714f5417f0",
        ),
        (
            "nix_expr",
            "1a3dfc4fce61c7fdf5d1b15cce6a91779df8e1fc8ffe8e649e70a972041b5c54",
        ),
        (
            "nix_store",
            "f11e2031be3da7741257679c2b835a6ca9c6bb7aed05ed5c293cde34458dcd27",
        ),
        (
            "nixos_test",
            "fdbb640a2f32518090d7f5be2e4333bcd2e4b20d1ac4de033483ba816896a672",
        ),
        (
            "html",
            "f4ca2f859046a283860f49b47ad82ba1ed42bc69294b6540e28bd6fcf6e9c845",
        ),
        (
            "markdown",
            "fcd939eb381234d3dfb3e8f2db761c9a7959a5ff39dd26ca9013a0693f17c2b0",
        ),
        (
            "latex",
            "8356fc7ab0943fa278900b379e65b7fa10b7e4cccd060355894fd02e31433256",
        ),
        (
            "text",
            "783594e04e69af1e63bdfd1b4c7d26c3b00735135c4dcb222953037a911cc111",
        ),
        (
            "sql",
            "d98950e57a6f2cab49a92beb708118b0f86183e26b810ffbc3b647606d6e24e1",
        ),
        (
            "haskell",
            "66667ba90aac7d754e2bfce210f36f046614c720ba51e05de0396afedb55b910",
        ),
        (
            "ocaml",
            "418d4bffa108dd7475c55b88a0356ade563415163a1260de625ae1b477cfad20",
        ),
        (
            "webassembly",
            "fb1fdee37816519ea63a06bea3a98030857c3e33d4d32c305aac9cd4e680b183",
        ),
        (
            "python",
            "eaca98374a864d55ccb8ab464d4aa1ef34470cd6dc065e0de902e41fbf9d9655",
        ),
        (
            "ubuntu_vm",
            "375097f43e8ae928870e95e54096d26a11546be0f71817323523bbc3f765ba77",
        ),
        (
            "bash",
            "937763643b6cc8b7cf83897b0627fc2bc4860b05fb9ef7e4ee596dde386d3493",
        ),
        (
            "shell",
            "1fbfd046d62fa62bbe90d2913b0bca52e481956633f47b54d2274565a18566de",
        ),
        (
            "rust",
            "f96d7fc986621b7c1453fb8f966bb6b128344a3cb816a5a426fec13c31f67005",
        ),
        (
            "racket",
            "45d053f8249c6ef81b5d785ca2200a3b1aa4c730511ae3d9401904047b991595",
        ),
        (
            "csharp",
            "8bdbb520f8b53b5d12010307847ad79d7b560e9efb59ee011e12cad9b054642c",
        ),
        (
            "c",
            "4b2beae8c2175941f0f5d68fba89460e68d9e1faec0107c0bdf3bdb93d36f7e8",
        ),
        (
            "cpp",
            "1e55fddd42f3a0c5e6db06e3d2cb2ac61c347fad48b2182936958bc37741ca1a",
        ),
        (
            "lisp",
            "ece45e7b377c9f255923884ddca788d7ffbfa52e84d17bd9fb47d1c0e7103d4f",
        ),
        (
            "common_lisp",
            "6491e355afdd00ed5d5bc07890ed051b3b03509d9bea7ca9c475d5ed63630335",
        ),
        (
            "ruby",
            "1fe5866e42fc211b2628cd06f582d57b8dfa00691f4b9b24de659b0b131ea023",
        ),
        (
            "matlab",
            "6ff94492952ad5fc3f8e1d20a3278a0fe2e9b5e03df8d068d40f17bef245a600",
        ),
        (
            "mathematica",
            "4e9cb060df4d0ffa91c8fa1b37479215cea3bfcdb266267a12c5ac95f057ace3",
        ),
        (
            "java",
            "9421ed01c9a024b23ed0f4ddc8acd17ecaa59096f30780d1862541c876c21afa",
        ),
        (
            "javascript",
            "8025c41424acbe27467b62d2a127145538b60490dfea5cef02f4254970417b42",
        ),
    ];

    const CATALOG_V6_SPEC_GOLDENS: &[(&str, &str)] = &[
        (
            "O",
            "227a4d1628315483efdcb91b6e38f952be392b4b4340b43fef5715fa1d434749",
        ),
        (
            "quote",
            "b762fa2acddd3324c6cbfa397b454c1c6ec47a9afee8087df7154be856e686c2",
        ),
        (
            "nix",
            "ff1a70056ad6d2bdf47e881885a41d1486222f1a71fbb1ad6d285cfde0d2e2c1",
        ),
        (
            "nix_expr",
            "c51aa2ff112b34b2605129ce6da079b12c7895e8c86486ba579fb5be7d638626",
        ),
        (
            "nix_store",
            "701958eac0969dea998735fb2c7df60e99ff38ad8be1f654486b326115bf2e78",
        ),
        (
            "nixos_test",
            "39ec104a945c55fc89f6cfdd4644594d89695c35a1f12bf2e37a93e26ad5d170",
        ),
        (
            "html",
            "6d042b09cce9968f3166517c3c0830449627b28a875880d2cfc46ba598dd29f0",
        ),
        (
            "markdown",
            "92e2bab0ea02c57f1c8eb9b39a710e0d33d902f574697469e69ee8e4029a38e3",
        ),
        (
            "latex",
            "e12d3c5e179601ca9a9fc9108c808defd8d0ade9f0d3c4da7ec39338a2378d2d",
        ),
        (
            "text",
            "f55342ed08820c51d894ff3b597f6064d42b39c02cd7289f49d6a01a19fefff7",
        ),
        (
            "sql",
            "39ce9c54a9624fa4e99644899339929e5cb0d756dd29486f100f80c7a2051e2a",
        ),
        (
            "haskell",
            "188548016654b5f0a23d3afc001fee0646243468cba599a43be14b15841e1fd8",
        ),
        (
            "ocaml",
            "af6b74f51a7b9c2485ab58ef7ae7c685052de07aba5a9e59d9b6d4a49a6924dc",
        ),
        (
            "webassembly",
            "28b2e9a07dd03c2ca1cad032d3753ab4c8d3652c0160680ccdfc4bdca02c1d3e",
        ),
        (
            "python",
            "e6111346a1d231844cc66eb1c9146aa7658e55c9fdb67f64f30491cab9121bcd",
        ),
        (
            "ubuntu_vm",
            "133be56de13eceef9b5613b33ddbe742513515a9f18b4af94d5be11838d496e5",
        ),
        (
            "bash",
            "8200dfb4d9618f68f71858b155b1302c3578d9c853b49f98fc393632b7685df4",
        ),
        (
            "shell",
            "b6a6c2e35f00e2f68edd092625c2fc08b0153cf083ec1f887c6756faf4dc8524",
        ),
        (
            "rust",
            "e936da6ec0878cfc8c60db386ba1e4d117d5afa1cdd61c4eb1453b725a13563c",
        ),
        (
            "racket",
            "da41cc733a04a65312934eb40cd42993ea099844cabd53f9b6a4624e3b35d8e6",
        ),
        (
            "csharp",
            "b37cfd1b87523c9208404dc96ec1f4c259b860ef8556b12de644b64f57eefaae",
        ),
        (
            "c",
            "ec8fbde941499da44f36c13ed474abf6ae6771edc105b076134e40c31d0f930d",
        ),
        (
            "cpp",
            "cad77fd041e2694a3904f353e9e70881b6e73a3ce1a914c55b348c2ffb24cd43",
        ),
        (
            "lisp",
            "14a79151290b298446fc713ae885aa3e5228f56990d419eaade86e61725b853e",
        ),
        (
            "common_lisp",
            "f0c0730bcf54edda1c09ada7abae9fc38cae0afee8ad5f62dea1a5b77cc8a77d",
        ),
        (
            "ruby",
            "7ea4ce9791e655b89a9b15cded5ea4ba3c53e90382bfba1bab21aabbf0a26a5e",
        ),
        (
            "matlab",
            "9dc68ae4c90a29dd298202a21ca6f043d356f3978eefd10f831ad73c180f3e21",
        ),
        (
            "mathematica",
            "ec10629c8c575af208fd5fbca20dadc71f9103e93b933d93b3c330cba020eaa0",
        ),
        (
            "java",
            "ce7a663d89c50a553982cd8fb420acafce45723296cb8c6c753fc373dea22daa",
        ),
        (
            "javascript",
            "cc4dd30821ca87daab896ddbc1e43f88b8399b79985fcad767400bb9085efa0a",
        ),
    ];

    fn artifact(byte: u8) -> ArtifactId {
        ArtifactId::from_sha256(format!("{byte:02x}").repeat(32)).unwrap()
    }

    fn digest(byte: u8) -> SemanticDigestV1 {
        SemanticDigestV1::from_sha256(format!("{byte:02x}").repeat(32)).unwrap()
    }

    fn executable_row(logical_command: &str, role: &str, byte: u8) -> BackendExecutableSetRowV2 {
        BackendExecutableSetRowV2::new(
            "bash",
            0,
            BackendExecutableSelectionV2::CompleteCatalogAlternative,
            logical_command,
            role,
            artifact(byte),
        )
        .unwrap()
    }

    #[test]
    fn catalog_v1_through_v5_archive_coordinates_are_explicit_and_v6_source_is_current() {
        let coordinates = [
            (
                "v1",
                "f923b2a8c2986d401d3efbc1da9ab7e077279af7",
                "905c1f9d3c551063c62a6046be72709237f4dfa00b26a47f477c1020cfe26897",
                "e03bcf77dfe4d4003046b7ceca3a79d8110a6d4060802a43b345d919e1108884",
                10_918,
            ),
            (
                "v2",
                "fd9d7c1a3844cab93a2d74c7261085bb227b5fe0",
                "589a34049e1984c1a0e1b957a43688e871fa3d4854318261dc88a947d73fd55c",
                "210c71503d64d876a3067dc0413ddb129677be9b24e985cde4adf9ec848eb57d",
                13_180,
            ),
            (
                "v3",
                "593611154adeb4f3f8323e8b4e85ad12e31625c6",
                "c2453ff4cb2480e03a4a0b2356439cbc2a0ddcc914ed948fa9fe91eab1ac79ea",
                "c489e11e27bf3b7677bcd14d91888978522ceb72ecea00374e2bf08bc3816d51",
                13_180,
            ),
            (
                "v4",
                "4f7d503dff73525b5d7f9c5b6a2f51c856bfe2e1",
                CATALOG_V4_WHOLE_SHA256,
                CATALOG_V4_SOURCE_SHA256,
                CATALOG_V4_SOURCE_BYTES,
            ),
            (
                "v5",
                "f6f830c475fe582c61d5c6ffd8fef29008c6ac34",
                CATALOG_V5_WHOLE_SHA256,
                CATALOG_V5_SOURCE_SHA256,
                CATALOG_V5_SOURCE_BYTES,
            ),
        ];
        let mut whole = BTreeSet::new();
        let mut source = BTreeSet::new();
        for (generation, commit, whole_sha256, source_sha256, source_bytes) in coordinates {
            assert_eq!(commit.len(), 40, "{generation} commit coordinate");
            assert!(commit.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert_eq!(whole_sha256.len(), 64, "{generation} whole digest");
            assert!(whole_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert_eq!(source_sha256.len(), 64, "{generation} source digest");
            assert!(source_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert!(source_bytes > 0);
            assert!(whole.insert(whole_sha256));
            assert!(source.insert(source_sha256));
        }
        assert!(whole.insert(CATALOG_V6_WHOLE_SHA256));
        assert!(source.insert(CATALOG_V6_SOURCE_SHA256));

        let registry = BackendRegistry::global();
        assert_eq!(registry.catalog_sha256_v3(), coordinates[2].2);
        assert_eq!(registry.catalog_sha256_v4(), coordinates[3].2);
        assert_eq!(registry.catalog_sha256_v5(), coordinates[4].2);
        assert_eq!(registry.catalog_sha256_v6(), CATALOG_V6_WHOLE_SHA256);
        assert_eq!(registry.catalog_sha256(), CATALOG_V6_WHOLE_SHA256);
        assert_ne!(registry.catalog_sha256(), coordinates[4].2);
        let current_source = include_bytes!("backend_catalog.inc.rs");
        assert_eq!(current_source.len(), CATALOG_V6_SOURCE_BYTES);
        assert_eq!(
            hex::encode(Sha256::digest(current_source)),
            CATALOG_V6_SOURCE_SHA256
        );
    }

    #[test]
    fn catalog_v3_through_v6_specification_goldens_cover_every_canonical_backend() {
        let registry = BackendRegistry::global();
        assert_eq!(
            registry.canonical_specs().len(),
            CATALOG_V3_SPEC_GOLDENS.len()
        );
        assert_eq!(
            registry.canonical_specs().len(),
            CATALOG_V4_SPEC_GOLDENS.len()
        );
        assert_eq!(
            registry.canonical_specs().len(),
            CATALOG_V5_SPEC_GOLDENS.len()
        );
        assert_eq!(
            registry.canonical_specs().len(),
            CATALOG_V6_SPEC_GOLDENS.len()
        );
        for (
            (((v3_name, v3_digest), (v4_name, v4_digest)), (v5_name, v5_digest)),
            (v6_name, v6_digest),
        ) in CATALOG_V3_SPEC_GOLDENS
            .iter()
            .zip(CATALOG_V4_SPEC_GOLDENS)
            .zip(CATALOG_V5_SPEC_GOLDENS)
            .zip(CATALOG_V6_SPEC_GOLDENS)
        {
            assert_eq!(v3_name, v4_name);
            assert_eq!(v4_name, v5_name);
            assert_eq!(v5_name, v6_name);
            assert_eq!(
                registry.specification_sha256_v3(v3_name).as_deref(),
                Some(*v3_digest)
            );
            assert_eq!(
                registry.specification_sha256_v4(v4_name).as_deref(),
                Some(*v4_digest)
            );
            assert_eq!(
                registry.specification_sha256_v5(v5_name).as_deref(),
                Some(*v5_digest)
            );
            assert_eq!(
                registry.specification_sha256_v6(v6_name).as_deref(),
                Some(*v6_digest)
            );
            assert_eq!(
                registry.specification_sha256(v6_name).as_deref(),
                Some(*v6_digest)
            );
            assert_ne!(v3_digest, v4_digest);
            assert_ne!(v4_digest, v5_digest);
            assert_ne!(v5_digest, v6_digest);
            assert!(!registry.contains_specification_sha256(v3_digest));
            assert!(!registry.contains_specification_sha256(v4_digest));
            assert!(!registry.contains_specification_sha256(v5_digest));
            assert!(registry.contains_specification_sha256(v6_digest));
        }
        assert_eq!(
            registry.specification_sha256_v4("py"),
            registry.specification_sha256_v4("python")
        );
        assert_eq!(
            registry.specification_sha256_v3("py"),
            registry.specification_sha256_v3("python")
        );
        assert_eq!(
            registry.specification_sha256_v5("py"),
            registry.specification_sha256_v5("python")
        );
        assert_eq!(
            registry.specification_sha256_v6("py"),
            registry.specification_sha256_v6("python")
        );
    }

    #[test]
    fn compatibility_catalog_paths_preserve_one_nominal_type_identity() {
        use std::any::TypeId;

        assert_eq!(
            TypeId::of::<BackendSpec>(),
            TypeId::of::<crate::ir::BackendSpec>()
        );
        assert_eq!(
            TypeId::of::<BackendSpec>(),
            TypeId::of::<crate::registry::bundle::BackendSpec>()
        );
        assert_eq!(
            TypeId::of::<BackendRegistry>(),
            TypeId::of::<crate::ir::BackendRegistry>()
        );
        assert_eq!(
            TypeId::of::<BackendRegistry>(),
            TypeId::of::<crate::registry::bundle::BackendRegistry>()
        );
        assert_eq!(
            TypeId::of::<BackendMorphismProfileV1>(),
            TypeId::of::<crate::backend_morphism::BackendMorphismProfileV1>()
        );
        assert_eq!(
            TypeId::of::<BackendMorphismProfileV1>(),
            TypeId::of::<crate::ir::BackendMorphismProfileV1>()
        );

        let canonical = BackendRegistry::global().get("python").unwrap();
        let ir_projection: &crate::ir::BackendSpec = canonical;
        let registry_projection: &crate::registry::bundle::BackendSpec = canonical;
        assert!(std::ptr::eq(canonical, ir_projection));
        assert!(std::ptr::eq(canonical, registry_projection));
    }

    #[test]
    fn executable_set_v2_is_order_invariant_and_content_sensitive() {
        let direct = executable_row("bash", "direct-launcher", 0x44);
        let proxy = executable_row("__ostadix_current_executable__", "ostadix-proxy", 0x55);
        let forward = backend_executable_set_v2([direct.clone(), proxy.clone()]).unwrap();
        let reverse = backend_executable_set_v2([proxy, direct]).unwrap();
        let changed = backend_executable_set_v2([
            executable_row("bash", "direct-launcher", 0x45),
            executable_row("__ostadix_current_executable__", "ostadix-proxy", 0x55),
        ])
        .unwrap();

        assert_eq!(forward, reverse);
        assert_ne!(forward, changed);
        assert_eq!(
            forward.as_sha256(),
            "f72db198d56a1b89e6daf7b9e43c345ed4790be49dbf28740bbcc66aaf2911a1"
        );
    }

    #[test]
    fn executable_set_v2_rejects_duplicate_launch_coordinates() {
        let error = backend_executable_set_v2([
            executable_row("bash", "direct-launcher", 0x44),
            executable_row("bash", "direct-launcher", 0x45),
        ])
        .unwrap_err();
        assert!(matches!(
            error,
            PlacementValidationError::Duplicate {
                kind: "backend executable-set coordinate",
                ..
            }
        ));
    }

    #[test]
    fn implementation_identity_aliases_resolve_to_one_canonical_backend() {
        let registry = BackendRegistry::global();
        let canonical = registry
            .backend_implementation_id_v1(
                "markdown",
                None,
                artifact(0x11),
                digest(0x22),
                LOCAL_BACKEND_PROTOCOL_ABI_V1,
            )
            .unwrap();
        let alias = registry
            .backend_implementation_id_v1(
                "md",
                None,
                artifact(0x11),
                digest(0x22),
                LOCAL_BACKEND_PROTOCOL_ABI_V1,
            )
            .unwrap();

        assert_eq!(canonical, alias);
        assert_eq!(
            canonical.backend_specification().as_sha256(),
            registry.specification_sha256("markdown").unwrap()
        );
        assert_eq!(
            canonical.realization_pipeline().as_sha256(),
            "1878587a74314b4376b1ec44c92ecb9b7781fbda3fd7d567f81f2222179665da"
        );

        // The catalog coordinate is a transitive input to the realization
        // pipeline. Preserve the exact V4 implementation identity as an
        // archival oracle while proving that the current V6 coordinate rolls
        // the derived identity forward.
        let v4_backend_specification = registry.specification_sha256_v4("markdown").unwrap();
        assert_eq!(
            v4_backend_specification,
            "01e3d22d55f365c4c7074b5b5ced2e04bfe15f96ee93e0f8758457d1c1e8c78b"
        );
        let v4_realization_material = serde_json::json!({
            "schema": LOCAL_REALIZATION_SCHEMA_V2,
            "backend_specification": v4_backend_specification,
            "adapter_kind": "inline",
            "adapter_artifact": "11".repeat(32),
            "executable_set_schema": BACKEND_EXECUTABLE_SET_DIGEST_DOMAIN_V2,
            "executable_set": "22".repeat(32),
            "protocol": LOCAL_BACKEND_PROTOCOL_ABI_V1,
        });
        let v4_realization_pipeline = SemanticDigestV1::hash_bytes(
            LOCAL_REALIZATION_DIGEST_DOMAIN_V2,
            &serde_json::to_vec(&v4_realization_material).unwrap(),
        );
        assert_eq!(
            v4_realization_pipeline.as_sha256(),
            "862eb13d45352d2ce3728700252c2b47ad237ced2521d38c4e315c5ad8af5a0b"
        );
        assert_ne!(canonical.realization_pipeline(), &v4_realization_pipeline);

        let v5_backend_specification = registry.specification_sha256_v5("markdown").unwrap();
        assert_eq!(
            v5_backend_specification,
            "fcd939eb381234d3dfb3e8f2db761c9a7959a5ff39dd26ca9013a0693f17c2b0"
        );
        let v5_realization_material = serde_json::json!({
            "schema": LOCAL_REALIZATION_SCHEMA_V2,
            "backend_specification": v5_backend_specification,
            "adapter_kind": "inline",
            "adapter_artifact": "11".repeat(32),
            "executable_set_schema": BACKEND_EXECUTABLE_SET_DIGEST_DOMAIN_V2,
            "executable_set": "22".repeat(32),
            "protocol": LOCAL_BACKEND_PROTOCOL_ABI_V1,
        });
        let v5_realization_pipeline = SemanticDigestV1::hash_bytes(
            LOCAL_REALIZATION_DIGEST_DOMAIN_V2,
            &serde_json::to_vec(&v5_realization_material).unwrap(),
        );
        assert_eq!(
            v5_realization_pipeline.as_sha256(),
            "4aeb615bec8522275e4424e6a7738186be568c5637b48b6cd8a413f80e83f6c4"
        );
        assert_ne!(canonical.realization_pipeline(), &v5_realization_pipeline);

        // Compatibility oracle for the formula formerly owned by
        // `o-registry::discover_backend_implementations`.
        let legacy_material = serde_json::json!({
            "schema": "ostadix.local-realization/v1",
            "backend_specification": registry.specification_sha256("markdown").unwrap(),
            "adapter_kind": "inline",
            "adapter_artifact": "11".repeat(32),
            "executable_set": "22".repeat(32),
            "protocol": "o-backend-cbor-v1",
        });
        let legacy_pipeline = SemanticDigestV1::hash_bytes(
            LOCAL_REALIZATION_DIGEST_DOMAIN_V1,
            &serde_json::to_vec(&legacy_material).unwrap(),
        );
        assert_eq!(
            legacy_pipeline.as_sha256(),
            "5794138c165548b9af8692d653baf760c4577a0bc61a7cedd1bbaa053f7a7a1a"
        );
        let v5_legacy_material = serde_json::json!({
            "schema": "ostadix.local-realization/v1",
            "backend_specification": v5_backend_specification,
            "adapter_kind": "inline",
            "adapter_artifact": "11".repeat(32),
            "executable_set": "22".repeat(32),
            "protocol": "o-backend-cbor-v1",
        });
        let v5_legacy_pipeline = SemanticDigestV1::hash_bytes(
            LOCAL_REALIZATION_DIGEST_DOMAIN_V1,
            &serde_json::to_vec(&v5_legacy_material).unwrap(),
        );
        assert_eq!(
            v5_legacy_pipeline.as_sha256(),
            "2c8faf4c5fa06d272893378c627f7ee276ef3235f35c97f0efca85f82c1fef36"
        );
        let v4_legacy_material = serde_json::json!({
            "schema": "ostadix.local-realization/v1",
            "backend_specification": v4_backend_specification,
            "adapter_kind": "inline",
            "adapter_artifact": "11".repeat(32),
            "executable_set": "22".repeat(32),
            "protocol": "o-backend-cbor-v1",
        });
        let v4_legacy_pipeline = SemanticDigestV1::hash_bytes(
            LOCAL_REALIZATION_DIGEST_DOMAIN_V1,
            &serde_json::to_vec(&v4_legacy_material).unwrap(),
        );
        assert_eq!(
            v4_legacy_pipeline.as_sha256(),
            "c98daed592288c5e4e80bd10be1c8f26786303d07d001d4250171616e4e41c90"
        );
        assert_ne!(legacy_pipeline, v4_legacy_pipeline);
        assert_ne!(legacy_pipeline, v5_legacy_pipeline);
        assert_ne!(canonical.realization_pipeline(), &legacy_pipeline);
        let legacy = BackendImplementationIdV1::new(
            canonical.backend_specification().clone(),
            canonical.adapter_artifact().clone(),
            canonical.executable_set().clone(),
            canonical.protocol_abi(),
            legacy_pipeline,
        )
        .unwrap();
        assert!(registry.contains_current_implementation(&canonical));
        assert!(!registry.contains_current_implementation(&legacy));
    }

    #[test]
    fn implementation_identity_rejects_unknown_and_stale_catalog_coordinates() {
        let registry = BackendRegistry::global();
        let unknown = registry
            .backend_implementation_id_v1(
                "not-a-current-backend",
                None,
                artifact(0x11),
                digest(0x22),
                LOCAL_BACKEND_PROTOCOL_ABI_V1,
            )
            .unwrap_err();
        assert!(matches!(
            unknown,
            PlacementValidationError::InvalidToken {
                field: "backend implementation canonical backend",
                ..
            }
        ));

        let stale = digest(0x33);
        let mismatch = registry
            .backend_implementation_id_v1(
                "markdown",
                Some(&stale),
                artifact(0x11),
                digest(0x22),
                LOCAL_BACKEND_PROTOCOL_ABI_V1,
            )
            .unwrap_err();
        assert_eq!(
            mismatch,
            PlacementValidationError::NonCurrentBackendCatalog {
                specification: stale.as_sha256().to_owned(),
                current_schema: BACKEND_CATALOG_CURRENT_SCHEMA.to_owned(),
            }
        );

        let wrong_current =
            SemanticDigestV1::from_sha256(registry.specification_sha256("html").unwrap()).unwrap();
        let mismatch = registry
            .backend_implementation_id_v1(
                "markdown",
                Some(&wrong_current),
                artifact(0x11),
                digest(0x22),
                LOCAL_BACKEND_PROTOCOL_ABI_V1,
            )
            .unwrap_err();
        assert!(matches!(
            mismatch,
            PlacementValidationError::ScopeMismatch {
                field: "backend implementation specification",
                ..
            }
        ));
    }

    #[test]
    fn implementation_identity_accepts_the_exact_current_catalog_coordinate() {
        let registry = BackendRegistry::global();
        let expected =
            SemanticDigestV1::from_sha256(registry.specification_sha256("markdown").unwrap())
                .unwrap();
        let implementation = registry
            .backend_implementation_id_v1(
                "md",
                Some(&expected),
                artifact(0x11),
                digest(0x22),
                LOCAL_BACKEND_PROTOCOL_ABI_V1,
            )
            .unwrap();

        assert_eq!(implementation.backend_specification(), &expected);
    }

    #[test]
    fn implementation_identity_rejects_a_foreign_protocol_abi() {
        let registry = BackendRegistry::global();
        let adapter_artifact = artifact(0x11);
        let executable_set = digest(0x22);
        let error = registry
            .backend_implementation_id_v1(
                "markdown",
                None,
                adapter_artifact.clone(),
                executable_set.clone(),
                "foreign-backend-abi-v1",
            )
            .unwrap_err();

        assert_eq!(
            error,
            PlacementValidationError::ScopeMismatch {
                field: "backend implementation protocol ABI",
                expected: LOCAL_BACKEND_PROTOCOL_ABI_V1.to_owned(),
                got: "foreign-backend-abi-v1".to_owned(),
            }
        );

        let backend_specification =
            SemanticDigestV1::from_sha256(registry.specification_sha256("markdown").unwrap())
                .unwrap();
        let foreign_material = serde_json::json!({
            "schema": LOCAL_REALIZATION_SCHEMA_V2,
            "backend_specification": backend_specification.as_sha256(),
            "adapter_kind": "inline",
            "adapter_artifact": adapter_artifact.as_sha256(),
            "executable_set_schema": BACKEND_EXECUTABLE_SET_DIGEST_DOMAIN_V2,
            "executable_set": executable_set.as_sha256(),
            "protocol": "foreign-backend-abi-v1",
        });
        let foreign_pipeline = SemanticDigestV1::hash_bytes(
            LOCAL_REALIZATION_DIGEST_DOMAIN_V2,
            &serde_json::to_vec(&foreign_material).unwrap(),
        );
        let foreign = BackendImplementationIdV1::new(
            backend_specification,
            adapter_artifact,
            executable_set,
            "foreign-backend-abi-v1",
            foreign_pipeline,
        )
        .unwrap();
        assert!(!registry.contains_current_implementation(&foreign));
    }

    #[test]
    fn registry_purity_is_conservative() {
        let reg = BackendRegistry::global();
        // The cache-safe set is limited to deterministic inline
        // representation handlers plus nix_expr's deterministic expression
        // *capture* (which never invokes a shim).
        for lang in ["nix_expr", "html", "markdown", "latex", "text"] {
            assert!(reg.is_pure(lang), "{lang} should be cache-safe");
        }
        // Every unrestricted shim-backed backend is impure: the runtime
        // does not enforce a closed deterministic execution environment,
        // so generic `{lazy}` caching would be unsound.
        for lang in [
            "nix",
            "nix_store",
            "nixos_test",
            "haskell",
            "ocaml",
            "webassembly",
            "python",
            "shell",
            "bash",
            "rust",
            "racket",
            "java",
            "javascript",
            "ruby",
            "sql",
            "O",
            "quote",
            "cobol",
        ] {
            assert!(!reg.is_pure(lang), "{lang} should be impure");
        }
    }

    /// The accepted tag set exposed by the registry contains every
    /// canonical backend name and every declared alias, with no duplicate
    /// or missing entries. Binaries derive their parser tag sets from this
    /// method instead of maintaining copies.
    #[test]
    fn registry_tag_set_covers_all_canonical_names_and_aliases() {
        let reg = BackendRegistry::global();
        let names = reg.registered_backend_names();

        // No duplicates.
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "duplicate tags in registry: {names:?}"
        );

        // Every canonical name and every alias is present.
        for spec in reg.canonical_specs() {
            assert!(
                unique.contains(spec.name),
                "missing canonical name {}",
                spec.name
            );
            for alias in spec.aliases {
                assert!(unique.contains(alias), "missing alias {alias}");
            }
        }

        // Every tag maps back to some spec (nothing extra).
        for tag in &names {
            assert!(reg.get(tag).is_some(), "tag {tag} resolves to no spec");
        }

        // Known aliases used by the parser remain accepted.
        for alias in ["py", "md", "tex", "plain", "o"] {
            assert!(unique.contains(alias), "parser alias {alias} must remain");
        }

        // Owned-tag convenience view agrees.
        let owned = reg.registered_backend_tags();
        assert_eq!(owned.len(), names.len());
    }

    #[test]
    fn canonical_catalog_links_every_backend_to_one_runtime_requirement() {
        let registry = BackendRegistry::global();
        let requirements = registry.runtime_requirement_specs();
        let requirement_keys = requirements
            .iter()
            .map(|requirement| requirement.key)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(requirement_keys.len(), requirements.len());

        let mut referenced = std::collections::BTreeSet::new();
        assert_eq!(registry.canonical_specs().len(), 30);
        for spec in registry.canonical_specs() {
            assert!(
                requirement_keys.contains(spec.runtime_requirement_key),
                "backend {} references missing runtime requirement {}",
                spec.name,
                spec.runtime_requirement_key
            );
            referenced.insert(spec.runtime_requirement_key);
            let requirement = registry.runtime_requirements_for(spec.name);
            match spec.adapter {
                BackendAdapterKind::Inline => {
                    assert_ne!(spec.execution, ExecutionMode::Shim, "{}", spec.name);
                    assert!(requirement.builtin, "{}", spec.name);
                    assert!(requirement.alternatives.is_empty(), "{}", spec.name);
                }
                BackendAdapterKind::NativeRust => {
                    if spec.execution == ExecutionMode::Shim {
                        assert!(!requirement.builtin, "{}", spec.name);
                    } else {
                        assert_eq!(spec.name, "nix_expr");
                        assert!(requirement.builtin, "{}", spec.name);
                    }
                }
                BackendAdapterKind::LegacyPythonShim => {
                    assert_eq!(spec.execution, ExecutionMode::Shim, "{}", spec.name);
                    assert!(!requirement.builtin, "{}", spec.name);
                    assert!(
                        requirement
                            .alternatives
                            .iter()
                            .any(|alternative| alternative.contains(&"python3")),
                        "legacy Python adapter {} must declare python3",
                        spec.name
                    );
                }
            }
        }
        assert_eq!(
            referenced, requirement_keys,
            "runtime requirement groups must not be orphaned"
        );
    }

    #[test]
    fn runtime_requirement_alternatives_preserve_or_of_and_semantics() {
        let registry = BackendRegistry::global();
        let rendered = |lang: &str| {
            registry
                .runtime_requirements_for(lang)
                .alternatives
                .iter()
                .map(|alternative| alternative.join("+"))
                .collect::<Vec<_>>()
                .join("|")
        };

        assert_eq!(rendered("java"), "javac+java");
        assert_eq!(rendered("haskell"), "runghc|ghc");
        assert_eq!(rendered("csharp"), "dotnet|mcs+mono");
        assert_eq!(
            rendered("webassembly"),
            "wat2wasm+wasmtime|wat2wasm+wasmer|wasm-tools+wasmtime|wasm-tools+wasmer"
        );
        assert_eq!(rendered("unregistered_backend"), "python3");
        assert_eq!(
            registry.runtime_requirements_for("webassembly").precision,
            RuntimeRequirementPrecision::ConservativeAllSources
        );
    }

    #[test]
    fn catalog_v6_expands_only_webassembly_runtime_alternatives_and_preserves_indices() {
        let registry = BackendRegistry::global();
        for current in registry.runtime_requirement_specs() {
            let archival = archival_runtime_requirement_v5(current);
            if current.key == "webassembly" {
                assert_eq!(
                    archival.alternatives,
                    &[&["wat2wasm", "wasmtime"][..], &["wat2wasm", "wasmer"][..]]
                );
                assert_eq!(&current.alternatives[..2], archival.alternatives);
                assert_eq!(
                    &current.alternatives[2..],
                    &[
                        &["wasm-tools", "wasmtime"][..],
                        &["wasm-tools", "wasmer"][..]
                    ]
                );
            } else {
                assert_eq!(current, archival);
            }
        }
    }

    #[test]
    fn catalog_adapter_projection_distinguishes_execution_implementations() {
        let registry = BackendRegistry::global();
        for lang in ["O", "quote", "html", "markdown", "latex", "text"] {
            assert_eq!(
                registry.adapter_for(lang),
                BackendAdapterKind::Inline,
                "{lang}"
            );
        }
        for lang in ["python", "py", "nixos_test", "ubuntu_vm", "ubuntu"] {
            assert_eq!(
                registry.adapter_for(lang),
                BackendAdapterKind::LegacyPythonShim,
                "{lang}"
            );
        }
        for lang in ["bash", "sql", "java", "webassembly", "common_lisp"] {
            assert_eq!(
                registry.adapter_for(lang),
                BackendAdapterKind::NativeRust,
                "{lang}"
            );
        }
        assert_eq!(
            registry.adapter_for("nix_expr"),
            BackendAdapterKind::NativeRust
        );
        assert_eq!(
            registry.adapter_for("unknown"),
            BackendAdapterKind::LegacyPythonShim
        );
    }

    #[test]
    fn catalog_digests_are_stable_canonical_projections() {
        let registry = BackendRegistry::global();
        assert_eq!(BACKEND_CATALOG_SCHEMA_V3, "ostadix.backend-catalog/v3");
        assert_eq!(BACKEND_CATALOG_SCHEMA_V4, "ostadix.backend-catalog/v4");
        assert_eq!(BACKEND_CATALOG_SCHEMA_V5, "ostadix.backend-catalog/v5");
        assert_eq!(BACKEND_CATALOG_SCHEMA_V6, "ostadix.backend-catalog/v6");
        assert_eq!(BACKEND_CATALOG_CURRENT_SCHEMA, BACKEND_CATALOG_SCHEMA_V6);
        assert_eq!(BACKEND_CATALOG_SCHEMA_V1, BACKEND_CATALOG_SCHEMA_V6);
        let catalog = registry.catalog_sha256();
        assert_eq!(catalog.len(), 64);
        assert!(catalog.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(catalog, registry.catalog_sha256());
        assert_eq!(catalog, registry.catalog_sha256_v6());
        assert_ne!(catalog, registry.catalog_sha256_v5());

        let python = registry.specification_sha256("python").unwrap();
        assert_eq!(python, registry.specification_sha256_v6("python").unwrap());
        assert_eq!(python, registry.specification_sha256("py").unwrap());
        assert_ne!(python, registry.specification_sha256("bash").unwrap());
        assert_eq!(python.len(), 64);
        assert!(registry.specification_sha256("unknown").is_none());

        assert!(registry.contains_specification_sha256(&python));
        assert!(
            registry.contains_specification_sha256(&registry.specification_sha256("py").unwrap())
        );
        assert!(!registry.contains_specification_sha256(&"0".repeat(64)));

        let legacy_v3 = registry.specification_sha256_v3("python").unwrap();
        assert_ne!(legacy_v3, python);
        assert_eq!(legacy_v3, registry.specification_sha256_v3("py").unwrap());
        assert!(!registry.contains_specification_sha256(&legacy_v3));
        let legacy_v4 = registry.specification_sha256_v4("python").unwrap();
        assert_ne!(legacy_v4, python);
        assert_eq!(legacy_v4, registry.specification_sha256_v4("py").unwrap());
        assert!(!registry.contains_specification_sha256(&legacy_v4));
        let legacy_v5 = registry.specification_sha256_v5("python").unwrap();
        assert_ne!(legacy_v5, python);
        assert_eq!(legacy_v5, registry.specification_sha256_v5("py").unwrap());
        assert!(!registry.contains_specification_sha256(&legacy_v5));
    }

    #[test]
    fn catalog_v3_digest_goldens_are_pinned_before_the_v4_rollover() {
        let registry = BackendRegistry::global();
        assert_eq!(
            registry.catalog_sha256_v3(),
            "c2453ff4cb2480e03a4a0b2356439cbc2a0ddcc914ed948fa9fe91eab1ac79ea"
        );
        let expected = [
            (
                "O",
                "d950d5857e1dc57ea4f2a2ae4603c22809aa3fbb0ae72fb983c78c5a9e632594",
            ),
            (
                "quote",
                "a354062b89cdc800361a22973e7b22029ef3a8ba6c89ae517dd060323fe9584d",
            ),
            (
                "nix",
                "af2e5cb7ca31a435f11e4d963fd72f1602bdc34af22f952e0fa2ab79d5073d4e",
            ),
            (
                "nix_expr",
                "6df666a6848122bf99bd6a6fb0a69ca807dec1324decf3e056a94af43ac6fe5c",
            ),
            (
                "nix_store",
                "de30341e99237888ae48d1acaad65d008281dd6d596b27a9b7bac8472888dd66",
            ),
            (
                "nixos_test",
                "3aab03df2cd680d7525038bb7d8f996abceabf88f0fc1b538f0555469863956f",
            ),
            (
                "html",
                "09c84ed2860b7e489ac7b85019bea022776201f777fa46342ffab3f906eff9ab",
            ),
            (
                "markdown",
                "aee4c1a29734ebfbf378b20e4fc19d7fbfb8365fbcbdd5538d337e3239a0c427",
            ),
            (
                "latex",
                "06ed79952fca6fb4e5221a9f4f9f36a15f17aaef747fdf7bae2fa290d497a6ab",
            ),
            (
                "text",
                "636b9d9237b58af152c2fe92896306e491f8cde407d01a1c0857b3a465f8b551",
            ),
            (
                "sql",
                "db4b71ab62c1c528f63b4b3706e7bab8a0e8583800d8a9cce27763f52ae618ec",
            ),
            (
                "haskell",
                "22f9325f9ca3200747b05cdee3656ffd65159fd06bfb55093956247451852f16",
            ),
            (
                "ocaml",
                "37cd484f614d3f8c1d1c3d06d8f52f9ecfe7d1d07b7728f7362536f3b07cf9bb",
            ),
            (
                "webassembly",
                "51cdefb6d3d187f6bd3a3c2ee343c18c462048e9e025d0d8052a9cad2bf4d4aa",
            ),
            (
                "python",
                "dd078f6b0eb48e099cce81b39711fa62313d39c7f8915abd97d9e72bc7678ecc",
            ),
            (
                "ubuntu_vm",
                "e863e96d5b3ce3e2b57a0ec8ee0ceb06038aa19b7929f5852ff2f92e117f44d3",
            ),
            (
                "bash",
                "e89ea6eb57eea53b50ba1c3b7a83a64c79d36160af7bffb0b15f8d03560c10cd",
            ),
            (
                "shell",
                "31652f34b475bdc7956505e39d135af74f80659f0795ca54a0e8f8a76c116cf9",
            ),
            (
                "rust",
                "8744827a7d497396ab645f4abd6f2f3a660da2fb769449d1cfa76ba98b68c2e5",
            ),
            (
                "racket",
                "c4e53bf39f937d0c25282b6f4e6a1ecf7073a68e110910fe0193cb446f2393e5",
            ),
            (
                "csharp",
                "4a34c14b79e3631831c220f61d8a30151b77fd4ccd475f35147aa94ba6d00e5e",
            ),
            (
                "c",
                "d8842139f0f671a062f414069d5653dc880e23e3439e73323868829fbaf7210d",
            ),
            (
                "cpp",
                "9c33364ebeb787d05f0ec4f01cf2f429cdcb6adbed2c01c67595557660dc6b73",
            ),
            (
                "lisp",
                "48df5c9240a148db43ad011dbf92a669138111540338273557e7d86faa8132fd",
            ),
            (
                "common_lisp",
                "7da0053792edf03b618f61408429bdc93fa2d50387f731c4d365d61d1f3b03c7",
            ),
            (
                "ruby",
                "14891c157518c0c8f34622adc00a8c755447ab7a894386341548dbec21fd6513",
            ),
            (
                "matlab",
                "d3917e3c2cd83292ef23795ba0098ccc62112c827a62499f404200c58c4bb8cf",
            ),
            (
                "mathematica",
                "c67b1715316b2b22349046e9541792d9d62c81bf48a9dd08b0d9fd2559557760",
            ),
            (
                "java",
                "b97392e28423d3aa4fd47919f18ca97d056b18344df9cba952f2c8a6c83b5962",
            ),
            (
                "javascript",
                "f98d171a8e35e67baa2d2a0b7d586140a1e37ea2d580844d596bf80ce8dd9bac",
            ),
        ];
        assert_eq!(registry.canonical_specs().len(), expected.len());
        for (name, digest) in expected {
            assert_eq!(
                registry.specification_sha256_v3(name).as_deref(),
                Some(digest)
            );
            assert!(!registry.contains_specification_sha256(digest));
        }
    }

    #[test]
    fn catalog_v4_declares_the_exact_state_support_partition() {
        use crate::placement::{BackendStateSupportV2, SnapshotCompatibilityV2};

        let registry = BackendRegistry::global();
        let mut stateless = Vec::new();
        let mut semantic = Vec::new();
        let mut external = Vec::new();
        for spec in registry.canonical_specs() {
            match &spec.state_support {
                BackendStateSupportV2::Stateless => stateless.push(spec.name),
                BackendStateSupportV2::SemanticSnapshot { .. } => semantic.push(spec.name),
                BackendStateSupportV2::ExternalPinned { .. } => external.push(spec.name),
            }
        }

        assert_eq!(stateless.len(), 27);
        assert_eq!(semantic, ["sql", "python"]);
        assert_eq!(external, ["ubuntu_vm"]);

        let expected_python_codec = crate::placement::SemanticDigestV1::hash_bytes(
            "ostadix/backend-state-codec-name/v2",
            b"ostadix.python-graph/v1",
        );
        assert_eq!(
            registry.state_support_for("py"),
            Some(&BackendStateSupportV2::SemanticSnapshot {
                codec: expected_python_codec,
                compatibility: SnapshotCompatibilityV2::ExactImplementation,
            })
        );
        let expected_sql_codec = crate::placement::SemanticDigestV1::hash_bytes(
            "ostadix/backend-state-codec-name/v2",
            crate::backend_state::SQL_CLI_CODEC_V1.as_bytes(),
        );
        assert_eq!(
            registry.state_support_for("sql"),
            Some(&BackendStateSupportV2::SemanticSnapshot {
                codec: expected_sql_codec,
                compatibility: SnapshotCompatibilityV2::ExactImplementation,
            })
        );
        let expected_ubuntu_manifest = crate::placement::SemanticDigestV1::hash_bytes(
            "ostadix/external-state-manifest-schema-name/v2",
            b"ostadix.multipass-resource/v1",
        );
        assert_eq!(
            registry.state_support_for("ubuntu"),
            Some(&BackendStateSupportV2::ExternalPinned {
                manifest_schema: expected_ubuntu_manifest,
            })
        );
        assert_eq!(registry.state_support_for("unknown"), None);
        assert_eq!(registry.interface_for("unknown").state_support, None);
    }

    #[test]
    fn catalog_v5_declares_the_exact_morphism_profile_partition_and_aliases() {
        let registry = BackendRegistry::global();
        assert_eq!(
            BACKEND_MORPHISM_PROFILE_ASSIGNMENTS.len(),
            registry.canonical_specs().len()
        );
        let mut assignment_names = BTreeSet::new();
        for (spec, (assigned_name, _)) in registry
            .canonical_specs()
            .iter()
            .zip(BACKEND_MORPHISM_PROFILE_ASSIGNMENTS)
        {
            assert_eq!(spec.name, *assigned_name);
            assert!(assignment_names.insert(*assigned_name));
        }
        let profiled = BACKEND_MORPHISM_PROFILE_ASSIGNMENTS
            .iter()
            .filter_map(|(name, profile)| profile.map(|profile| (*name, profile)))
            .collect::<Vec<_>>();
        assert_eq!(
            profiled,
            [
                ("python", BackendMorphismProfileV1::PythonPlainData),
                ("rust", BackendMorphismProfileV1::RustSourceConstantStdout),
                (
                    "javascript",
                    BackendMorphismProfileV1::JavascriptBindingStdout,
                ),
            ]
        );
        assert_eq!(
            BACKEND_MORPHISM_PROFILE_ASSIGNMENTS
                .iter()
                .filter(|(_, profile)| profile.is_none())
                .count(),
            27
        );
        assert_eq!(
            registry.morphism_profile_for("py"),
            Some(BackendMorphismProfileV1::PythonPlainData)
        );
        assert_eq!(registry.morphism_profile_for("html"), None);
        assert_eq!(registry.morphism_profile_for("unknown"), None);
        assert_eq!(
            serde_json::to_string(&BackendMorphismProfileV1::JavascriptBindingStdout).unwrap(),
            "\"javascript-binding-stdout\""
        );
    }

    #[test]
    fn catalog_v5_hashes_the_profile_partition_while_v4_stays_archival() {
        let registry = BackendRegistry::global();
        let python = registry.get("python").unwrap();
        let requirement = registry.runtime_requirements_for("python");
        let v4_digest_for = |_profile: Option<BackendMorphismProfileV1>| {
            let mut hash = Sha256::new();
            catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V4.as_bytes());
            hash_backend_spec_v4(&mut hash, python, requirement);
            finish_catalog_hash(hash)
        };
        let v5_digest_for = |profile| {
            let mut hash = Sha256::new();
            catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V5.as_bytes());
            hash_backend_spec_v5(&mut hash, python, requirement, profile);
            finish_catalog_hash(hash)
        };

        let direct_v4 = v4_digest_for(Some(BackendMorphismProfileV1::PythonPlainData));
        assert_eq!(
            direct_v4,
            v4_digest_for(None),
            "archival V4 identity predates morphism profiles"
        );
        assert_eq!(
            direct_v4,
            v4_digest_for(Some(BackendMorphismProfileV1::JavascriptBindingStdout)),
            "archival V4 identity must not accept a profile input"
        );
        assert_eq!(
            direct_v4,
            CATALOG_V4_SPEC_GOLDENS
                .iter()
                .find_map(|(name, digest)| (*name == "python").then_some(*digest))
                .unwrap()
        );

        let direct_v5 = v5_digest_for(Some(BackendMorphismProfileV1::PythonPlainData));
        assert_eq!(
            direct_v5,
            CATALOG_V5_SPEC_GOLDENS
                .iter()
                .find_map(|(name, digest)| (*name == "python").then_some(*digest))
                .unwrap()
        );
        let absent_v5 = v5_digest_for(None);
        let javascript_v5 = v5_digest_for(Some(BackendMorphismProfileV1::JavascriptBindingStdout));
        assert_ne!(direct_v5, absent_v5);
        assert_ne!(direct_v5, javascript_v5);
        assert_ne!(absent_v5, javascript_v5);
    }

    #[test]
    fn catalog_v4_hashes_state_support_while_v3_stays_archival() {
        use crate::placement::BackendStateSupportV2;

        let registry = BackendRegistry::global();
        let python = registry.get("python").unwrap();
        let requirement = registry.runtime_requirements_for("python");
        let digest_for =
            |schema: &str,
             spec: &BackendSpec,
             hash_spec: fn(&mut Sha256, &BackendSpec, &RuntimeRequirementSpec)| {
                let mut hash = Sha256::new();
                catalog_hash_field(&mut hash, schema.as_bytes());
                hash_spec(&mut hash, spec, requirement);
                finish_catalog_hash(hash)
            };

        let mut weakened = python.clone();
        weakened.state_support = BackendStateSupportV2::Stateless;
        assert_eq!(
            digest_for(BACKEND_CATALOG_SCHEMA_V3, python, hash_backend_spec_v3),
            digest_for(BACKEND_CATALOG_SCHEMA_V3, &weakened, hash_backend_spec_v3),
            "archival V3 identity predates state support"
        );
        let direct_v4 = digest_for(BACKEND_CATALOG_SCHEMA_V4, python, hash_backend_spec_v4);
        assert_eq!(
            direct_v4,
            CATALOG_V4_SPEC_GOLDENS
                .iter()
                .find_map(|(name, digest)| (*name == "python").then_some(*digest))
                .unwrap(),
            "the internal V4 hash helper must retain the published Python anchor"
        );
        assert_ne!(
            direct_v4,
            digest_for(BACKEND_CATALOG_SCHEMA_V4, &weakened, hash_backend_spec_v4),
            "archival V4 identity must bind state support"
        );
    }

    #[test]
    fn exact_range_catalog_syntax_hashes_canonical_bigint_bounds() {
        let parsed = integer_exactness!(ExactRange {
            min: "-10",
            max: "20"
        });
        assert_eq!(
            parsed,
            IntegerExactness::ExactRange {
                min: BigInt::from(-10),
                max: BigInt::from(20),
            }
        );

        let registry = BackendRegistry::global();
        let digest_for = |integer_exactness: IntegerExactness| {
            let mut spec = registry.get("javascript").unwrap().clone();
            spec.value_capabilities.integer_exactness = integer_exactness;
            let mut hash = Sha256::new();
            catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V3.as_bytes());
            hash_backend_spec_v3(
                &mut hash,
                &spec,
                registry.runtime_requirements_for_v5(spec.name),
            );
            finish_catalog_hash(hash)
        };

        let canonical = IntegerExactness::ExactRange {
            min: BigInt::from(-10),
            max: BigInt::from(20),
        };
        assert_eq!(digest_for(parsed), digest_for(canonical));
        assert_ne!(
            digest_for(IntegerExactness::ExactRange {
                min: BigInt::from(-10),
                max: BigInt::from(20),
            }),
            digest_for(IntegerExactness::ExactRange {
                min: BigInt::from(-10),
                max: BigInt::from(21),
            })
        );
        assert_ne!(
            digest_for(IntegerExactness::ExactMagnitudeBits(63)),
            digest_for(IntegerExactness::TwosComplementBits(63))
        );
    }

    #[test]
    #[should_panic(expected = "must use canonical signed base-10 spelling")]
    fn exact_range_catalog_syntax_rejects_noncanonical_bounds() {
        let _ = integer_exactness!(ExactRange {
            min: "-00010",
            max: "20"
        });
    }

    #[test]
    fn catalog_value_capabilities_follow_canonical_backend_identity() {
        let registry = BackendRegistry::global();
        assert_eq!(
            registry.value_capabilities_for("python"),
            registry.value_capabilities_for("py")
        );
        assert_eq!(
            registry.value_capabilities_for("python"),
            BackendValueCapabilities {
                integer_exactness: IntegerExactness::Arbitrary,
                rich_numbers: RichNumberPreservation::Preserved,
            }
        );
        assert_eq!(
            registry.value_capabilities_for("javascript"),
            BackendValueCapabilities {
                integer_exactness: IntegerExactness::ExactMagnitudeBits(53),
                rich_numbers: RichNumberPreservation::Collapsed,
            }
        );
        assert_eq!(
            registry.value_capabilities_for("java"),
            BackendValueCapabilities {
                integer_exactness: IntegerExactness::TwosComplementBits(63),
                rich_numbers: RichNumberPreservation::Collapsed,
            }
        );
        assert_eq!(
            registry.value_capabilities_for("unregistered"),
            BackendValueCapabilities::UNKNOWN
        );

        let python = registry.interface_for("py");
        assert_eq!(python.canonical, "python");
        assert_eq!(
            python.specification_sha256,
            registry.specification_sha256("python")
        );
        assert_eq!(
            python.value_capabilities,
            registry.value_capabilities_for("python")
        );
    }

    #[test]
    fn registry_renderers_match_legacy_dispatch() {
        let reg = BackendRegistry::global();
        assert_eq!(reg.renderer_for("python"), SpliceRenderer::Python);
        assert_eq!(reg.renderer_for("py"), SpliceRenderer::Python);
        assert_eq!(reg.renderer_for("html"), SpliceRenderer::Html);
        assert_eq!(reg.renderer_for("latex"), SpliceRenderer::Latex);
        assert_eq!(reg.renderer_for("tex"), SpliceRenderer::Latex);
        assert_eq!(reg.renderer_for("markdown"), SpliceRenderer::Markdown);
        assert_eq!(reg.renderer_for("md"), SpliceRenderer::Markdown);
        assert_eq!(reg.renderer_for("nix"), SpliceRenderer::Nix);
        assert_eq!(reg.renderer_for("nix_store"), SpliceRenderer::Nix);
        assert_eq!(reg.renderer_for("nixos_test"), SpliceRenderer::Nix);
        // nix_expr splices via the default representation (legacy behavior).
        assert_eq!(reg.renderer_for("nix_expr"), SpliceRenderer::Default);
        assert_eq!(reg.renderer_for("cobol"), SpliceRenderer::Default);
    }

    #[test]
    fn catalog_exposes_adapter_required_authority() {
        let reg = BackendRegistry::global();
        assert!(reg.interface_for("python").required_authorities.is_empty());
        assert_eq!(
            reg.interface_for("bash").required_authorities,
            vec![BackendAuthority::Process]
        );
        assert_eq!(
            reg.interface_for("nix").required_authorities,
            BackendAuthority::ALL
        );
        assert_eq!(
            reg.interface_for("unregistered_backend")
                .required_authorities,
            BackendAuthority::ALL,
            "unknown shims must default to the conservative authority envelope"
        );
    }

    #[test]
    fn shim_resolution_falls_back_to_convention() {
        let reg = BackendRegistry::global();
        let dir = Path::new("/nonexistent_shim_dir_for_test");
        assert_eq!(
            reg.resolve_shim_path(dir, "python"),
            dir.join("python_shim.py")
        );
    }

    #[test]
    fn catalog_exposes_typed_backend_interface() {
        let reg = BackendRegistry::global();
        let python = reg.interface_for("py");
        let html = reg.interface_for("html");
        let quote = reg.interface_for("quote");

        assert_eq!(python.canonical, "python");
        assert_eq!(python.execution, ExecutionMode::Shim);
        assert_eq!(html.execution, ExecutionMode::InlineValue);
        assert_eq!(quote.execution, ExecutionMode::InlineAst);
    }
}
