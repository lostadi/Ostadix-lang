//! Canonical compiled backend catalog and exact implementation metadata.
//!
//! This module is the one source of truth for backend aliases, adapters,
//! runtime requirements, value fidelity, state support, and catalog identity.
//! The placement protocol depends only on an injected catalog interface; this
//! bundle supplies the process-wide current implementation.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, OnceLock};

use num_bigint::BigInt;
use sha2::{Digest, Sha256};

use crate::placement::protocol::{
    BackendStateSupportV2, CurrentBackendCatalogV1, SemanticDigestV1, SnapshotCompatibilityV2,
};
use crate::value::BackendAuthority;

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
/// renderer functions live in eval.rs (they need OValue); the registry only
/// records which strategy a backend uses, so the dispatch decision is
/// centralized here while the value-level code stays with the evaluator.
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
    /// Syntactically valid Nix expressions.
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
    /// Implemented by `src/backend.rs` inside the current Ostadix executable.
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
        legacy_schema_v3: $legacy_schema_v3:literal $(,)?
    ) => {
        /// Legacy catalog domain retained for archival V3 inspection.
        pub const BACKEND_CATALOG_SCHEMA_V3: &str = $legacy_schema_v3;
        /// Current catalog domain. Only identities derived under this domain
        /// authorize new placement records.
        pub const BACKEND_CATALOG_SCHEMA_V4: &str = $current_schema;
        pub const BACKEND_CATALOG_CURRENT_SCHEMA: &str = BACKEND_CATALOG_SCHEMA_V4;
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

#[cfg(test)]
pub(crate) use integer_exactness;

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
    };
}

// The included file is pure declarative data and is also embedded verbatim by
// olangc so emitted runtime projects compile from the identical catalog.
include!("../../backend_catalog.inc.rs");

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
    // V4 is the exact V3 projection extended by one explicit state-support
    // field. Keeping the shared prefix makes the rollover auditable while the
    // distinct schema domain prevents cross-version authorization.
    hash_backend_spec_v3(hash, spec, requirement);
    hash_state_support(hash, &spec.state_support);
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
                    hash_runtime_requirement(&mut hash, requirement);
                }
                catalog_hash_count(&mut hash, self.specs.len());
                for spec in self.specs {
                    let requirement = self.runtime_requirements_for(spec.name);
                    hash_backend_spec_v3(&mut hash, spec, requirement);
                }
                finish_catalog_hash(hash)
            })
            .clone()
    }

    /// Deterministic SHA-256 of the complete current ordered catalog.
    pub fn catalog_sha256(&self) -> String {
        static CURRENT_DIGEST: OnceLock<String> = OnceLock::new();
        CURRENT_DIGEST
            .get_or_init(|| {
                let mut hash = Sha256::new();
                catalog_hash_field(&mut hash, BACKEND_CATALOG_CURRENT_SCHEMA.as_bytes());
                catalog_hash_count(&mut hash, RUNTIME_REQUIREMENT_SPECS.len());
                for requirement in RUNTIME_REQUIREMENT_SPECS {
                    hash_runtime_requirement(&mut hash, requirement);
                }
                catalog_hash_count(&mut hash, self.specs.len());
                for spec in self.specs {
                    hash_backend_spec_v4(&mut hash, spec, self.runtime_requirements_for(spec.name));
                }
                finish_catalog_hash(hash)
            })
            .clone()
    }

    /// Deterministic SHA-256 of one canonical backend specification and its
    /// referenced runtime requirements. Aliases resolve to the same digest.
    pub fn specification_sha256_v3(&self, lang: &str) -> Option<String> {
        let spec = self.get(lang)?;
        let mut hash = Sha256::new();
        catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V3.as_bytes());
        hash_backend_spec_v3(&mut hash, spec, self.runtime_requirements_for(spec.name));
        Some(finish_catalog_hash(hash))
    }

    /// Deterministic SHA-256 of one current canonical backend specification.
    /// Aliases resolve to the same exact implementation identity.
    pub fn specification_sha256(&self, lang: &str) -> Option<String> {
        let spec = self.get(lang)?;
        let mut hash = Sha256::new();
        catalog_hash_field(&mut hash, BACKEND_CATALOG_CURRENT_SCHEMA.as_bytes());
        hash_backend_spec_v4(&mut hash, spec, self.runtime_requirements_for(spec.name));
        Some(finish_catalog_hash(hash))
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
