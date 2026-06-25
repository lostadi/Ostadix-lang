// ─────────────────────────────────────────────────────────────────────────────
// value.rs
//
// The OValue type system — the universal intermediate representation of the O
// language runtime. Every value that crosses a language boundary in an O
// program is an OValue. No exceptions.
//
// This file has zero dependencies on parsing, evaluation, or process
// management. It is the pure data layer. It answers one question: what IS a
// value in O?
//
// The early runtime was intentionally JSON-shaped. The public model now grows
// toward a typed, canonical, content-addressed value graph while preserving the
// old wire tags as compatibility forms for existing shims.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::str::FromStr;

use anyhow::{bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use hex;
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod bigint_json {
    use super::*;
    use serde::de::{self, Visitor};

    pub fn serialize<S>(value: &BigInt, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BigInt, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct BigIntVisitor;

        impl Visitor<'_> for BigIntVisitor {
            type Value = BigInt;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a decimal integer string or JSON integer")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                BigInt::from_str(value).map_err(E::custom)
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.visit_str(&value)
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(BigInt::from(value))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(BigInt::from(value))
            }
        }

        deserializer.deserialize_any(BigIntVisitor)
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// SECTION 1: The OValue Sum Type
// ═════════════════════════════════════════════════════════════════════════════

/// Lossless numeric forms used by the post-MVP value model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ONumber {
    Int {
        #[serde(with = "bigint_json")]
        v: BigInt,
    },
    Rational {
        #[serde(with = "bigint_json")]
        num: BigInt,
        #[serde(with = "bigint_json")]
        den: BigInt,
    },
    Decimal {
        #[serde(with = "bigint_json")]
        coeff: BigInt,
        exp10: i64,
        special: Option<DecimalSpecial>,
    },
    BinaryFloat {
        format: FloatFormat,
        bits: Vec<u8>,
    },
    BigFloat {
        #[serde(with = "bigint_json")]
        mantissa: BigInt,
        exp2: i64,
        precision: Option<u64>,
        special: Option<FloatSpecial>,
    },
    Complex {
        re: Box<ONumber>,
        im: Box<ONumber>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecimalSpecial {
    Nan,
    PosInf,
    NegInf,
    PosZero,
    NegZero,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FloatFormat {
    F32,
    F64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FloatSpecial {
    Nan,
    PosInf,
    NegInf,
    PosZero,
    NegZero,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OText {
    pub utf8: String,
    pub encoding: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OBytes {
    pub bytes: Vec<u8>,
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeqKind {
    List,
    Tuple,
    Vector,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetKind {
    Ordered,
    Unordered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OSymbol {
    pub namespace: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OKeyword {
    pub namespace: Option<String>,
    pub name: String,
}

pub type NodeId = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphNode {
    Value { value: Box<OValue> },
    Ref { target: NodeId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeIdentity {
    pub stable: Option<String>,
    pub live: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCodecSafety {
    DataOnly,
    SourceBacked,
    UnsafeOpaque,
    LiveHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeBoundary {
    Pure,
    Referential,
    Effectful,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RehydratePolicy {
    Portable,
    SameBackend,
    SameProcess,
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ONative {
    pub lang: String,
    pub implementation: Option<String>,
    pub version: Option<String>,
    pub type_name: String,
    pub identity: NativeIdentity,
    pub codec: String,
    pub payload: Option<OBytes>,
    pub boundary: NativeBoundary,
    pub safety: NativeCodecSafety,
    pub capabilities: Vec<CapabilityKind>,
    pub metadata: BTreeMap<String, OValue>,
    pub rehydrate: RehydratePolicy,
}

/// The complete universe of values in the O language runtime.
///
/// Legacy variants (`Int`, `Float`, `Str`, `List`, and string-keyed `Map`) stay
/// as compatibility forms. New backends can target the richer structural forms
/// (`Number`, `Text`, `Bytes`, `Seq`, `Object`, `EntriesMap`, `Graph`, and
/// `Native`) without losing information before the rest of the evaluator learns
/// their full semantics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum OValue {
    /// The absence of a value. Distinct from false and from zero.
    /// Produced by: void expressions, cleanup operations, null returns.
    Null,

    /// A boolean. No implicit coercions — true is true, false is false.
    #[serde(rename = "bool")]
    Bool { v: bool },

    /// A 64-bit signed integer.
    /// Known limitation: Python/Haskell arbitrary precision integers that
    /// exceed i64::MAX will lose precision. To be fixed with num-bigint.
    #[serde(rename = "int")]
    Int { v: i64 },

    /// A 64-bit IEEE 754 floating point number.
    #[serde(rename = "float")]
    Float { v: f64 },

    /// A lossless numeric value: big integers, rationals, decimal/binary
    /// floats, big floats, and complex numbers.
    #[serde(rename = "number")]
    Number { v: ONumber },

    /// A UTF-8 string. The most common inter-language value type.
    /// Raw text from backends, spliced $var values, document content —
    /// all arrive as OStr unless the backend explicitly returns something richer.
    #[serde(rename = "str")]
    Str { v: String },

    /// Text with explicit encoding metadata. `Str` remains the compatibility
    /// projection for existing shims.
    #[serde(rename = "text")]
    Text { v: OText },

    /// A single Unicode scalar value.
    #[serde(rename = "char")]
    Char { scalar: char },

    #[serde(rename = "html")]
    Html { v: String },
    #[serde(rename = "store_path")]
    StorePath { path: String },

    /// A captured but unevaluated O expression — the homoiconicity value.
    ///
    /// Produced by `quote^(...)_quote`. The `src` field holds the
    /// reconstructed O source text of the quoted body (lossless enough
    /// for re-evaluation via `O.eval`). Nothing inside was executed at
    /// capture time.
    ///
    /// Wire format: `{"t":"expr","src":"<O source text>"}`
    ///
    /// In Python shims, an Expr value is bound as an `OExprValue` Python
    /// object; `O.eval(q)` re-enters the Rust runtime via the eval_request
    /// callback protocol to evaluate it against the live document context.
    #[serde(rename = "expr")]
    Expr { src: String },

    /// An ordered, heterogeneous sequence of OValues.
    /// Python lists, Haskell lists, JSON arrays, Racket lists — all map here.
    #[serde(rename = "list")]
    List { v: Vec<OValue> },

    /// A string-keyed map of OValues.
    /// Python dicts, JSON objects, Racket hash tables — all map here.
    /// Keys are ALWAYS strings at the O level. Non-string keys in source
    /// languages must be stringified by their backend shim.
    #[serde(rename = "map")]
    Map { v: HashMap<String, OValue> },

    /// A sequence whose source-language shape matters.
    #[serde(rename = "seq")]
    Seq { kind: SeqKind, items: Vec<OValue> },

    /// A structural object with deterministic string fields.
    #[serde(rename = "object")]
    Object { fields: BTreeMap<String, OValue> },

    /// A map whose keys are arbitrary OValues. This is the post-MVP map shape;
    /// legacy `Map` stays as the JSON-object compatibility projection.
    #[serde(rename = "entries_map")]
    EntriesMap { entries: Vec<(OValue, OValue)> },

    /// A set that preserves source-language ordered/unordered intent.
    #[serde(rename = "set")]
    Set { kind: SetKind, items: Vec<OValue> },

    #[serde(rename = "symbol")]
    Symbol { v: OSymbol },

    #[serde(rename = "keyword")]
    Keyword { v: OKeyword },

    /// A first-class snapshot of an O-level lexical scope.
    ///
    /// A Scope is not a plain Map. It records that its entries are bindings to
    /// use as the lexical root of a later evaluation, most notably
    /// `O.eval(expr, scope)`. The snapshot is detached: evaluation may read and
    /// shadow its bindings, but cannot mutate the scope from which it was made.
    ///
    /// Scope snapshots may contain capabilities and live references, so they
    /// are conservatively non-cacheable, non-replayable, and non-persistable.
    #[serde(rename = "scope")]
    Scope { bindings: HashMap<String, OValue> },

    /// Raw binary data with a MIME type hint for the receiving backend.
    ///
    /// This is the escape hatch for rich values: matplotlib figures,
    /// compiled PDFs, rendered HTML, audio, video, arbitrary binary.
    /// The MIME type carries the rendering semantics:
    ///   "image/png"        → HTML backend renders as <img src="data:...">
    ///   "text/html"        → HTML backend embeds fragment directly
    ///   "application/pdf"  → rendered to file by the output pipeline
    ///
    /// Data is base64-encoded on the wire. The `v` field carries the base64
    /// string; `mime` is a separate field (not inside `v`) because both are
    /// required and neither is "the value" more than the other.
    #[serde(rename = "blob")]
    Blob { v: String, mime: String }, // v = base64-encoded bytes on wire

    /// Proper bytes. `Blob` remains the MIME-bearing compatibility carrier used
    /// by renderers; `Bytes` is the structural byte value.
    #[serde(rename = "bytes")]
    Bytes { v: OBytes },

    /// A graph frame for values with shared identity, cycles, or explicit refs.
    #[serde(rename = "graph")]
    Graph { root: NodeId, nodes: Vec<GraphNode> },

    /// A language-native value capsule. Structural values should be preferred
    /// whenever O understands the value; native capsules are for honest escape
    /// hatches that only the source backend can decode.
    #[serde(rename = "native")]
    Native { v: ONative },

    /// A lazy Nix expression that has not yet been passed to `nix eval`.
    ///
    /// This is the "deferred drv rung" value produced by `nix_expr^(...)_nix_expr`
    /// blocks. It holds:
    /// - `body`: fully spliced Nix source ready for `nix eval`.
    /// - `deps`: child OValues whose rendered forms were spliced into the body.
    /// - `fingerprint`: SHA-256 over the body and sorted dependency identities.
    ///   This composes with Nix content identities and remains independent of
    ///   dependency traversal order.
    ///
    /// `nix^(...)_nix` is unchanged — it is the "evaluate immediately to a JSON value"
    /// shortcut that bypasses this rung entirely (step 1 decision, option a).
    #[serde(rename = "nix_expr")]
    NixExpr {
        body: String,
        deps: Vec<OValue>,
        fingerprint: String,
    },

    /// A Nix derivation that has been instantiated but not yet realised.
    ///
    /// This is the MIDDLE RUNG — produced by `instantiate(expr)`. The .drv file
    /// exists in the Nix store; the outputs do not yet. Identity is `drv_path`
    /// (already content-addressed by Nix itself — we don't re-hash).
    ///
    /// Step-2 simplification: only the first output (`out` by default) is exposed
    /// after realisation. Multi-output realise (returning OMap of {name: store_path})
    /// is deferred — see eval.rs::realise_to_first_output.
    ///   STEP3: support `realise(drv, "dev")` for selecting a non-default output,
    ///   or `realise_all(drv)` returning an OMap of all outputs.
    #[serde(rename = "derivation")]
    Derivation {
        drv_path: String,     // /nix/store/*.drv — canonical identity
        outputs: Vec<String>, // ["out", "dev", ...] (parsed from `nix derivation show`)
        deps: Vec<OValue>,    // child OValues that contributed to this drv (provenance)
    },

    /// A request to perform a deferred computation.
    ///
    /// Requests are FIRST-CLASS VALUES: they can be bound, passed, composed.
    /// A Request with `source: Box<OValue::Request>` represents a chained climb
    /// (e.g. `realise(instantiate(expr))`); the executor walks the chain.
    ///
    /// `fingerprint = sha256(kind_tag || source.content_identity())`. This
    /// composes: two requests with the same kind over sources with the same
    /// content identity have the same fingerprint, so the executor's cache
    /// hits on logical equality regardless of how the request was constructed.
    ///
    /// Auto-resolution fires at Request *construction* time inside eval_call,
    /// based on the policy in effect AT CONSTRUCTION. Once a Request escapes
    /// (because it was constructed under Policy::Lazy), no downstream context
    /// re-fires auto-resolve. The user must explicitly force with `now(...)`.
    #[serde(rename = "request")]
    Request {
        kind: RequestKind,
        source: Box<OValue>,
        fingerprint: String,
    },

    /// STEP-4: a reference to a running operating system.
    ///
    /// This is the FIRST OValue that refers to something IN THE WORLD rather
    /// than a content-addressed snapshot. The `profile_path` is a symlink in
    /// the Nix store (typically `/nix/var/nix/profiles/system` for the
    /// system-wide profile, or `~/.nix-profile` for a user profile). The
    /// symlink's target — the currently-active generation — can change at
    /// any time, by:
    ///   - this runtime (via an Activate request)
    ///   - other Nix tooling running outside O-lang
    ///   - the user manually rolling back
    ///   - automatic rollbacks (boot-time generation selection)
    ///
    /// Consequence: equality of two Systems is profile_path equality, NOT
    /// state equality. A System carries a reference, not a snapshot. Querying
    /// current state (which generation, which kernel, etc.) requires an
    /// out-of-band call. This is the structural concession that lets the OS
    /// participate in the value model without pretending to be pure.
    ///
    /// STEP5: extend with `kind` so non-NixOS profiles (Home-Manager, nix-darwin,
    /// per-user) can be distinguished. Add Snapshot value type for captured
    /// state at a specific generation. Add Rollback / Update transitions.
    #[serde(rename = "system")]
    System { profile_path: String },

    /// A capability-scoped reference to a privileged system resource.
    ///
    /// Capabilities are the runtime's explicit representation of authority:
    /// instead of assuming ambient access to files, devices, clocks, network
    /// sockets, or services, O can pass a first-class value that says WHAT
    /// resource is being authorized and which descriptive metadata travels with
    /// it.
    ///
    /// `kind` identifies the resource class. For live capabilities, `identity`
    /// is an opaque session bearer resolved only through a private authority
    /// table. `metadata` is descriptive and never grants authority.
    #[serde(rename = "capability")]
    Capability {
        kind: CapabilityKind,
        identity: String,
        metadata: HashMap<String, OValue>,
    },

    /// A persisted snapshot of world state captured at a specific observation
    /// boundary.
    ///
    /// Unlike `System`, which is a live reference to a moving profile path, a
    /// Snapshot is inert data: it names a captured state (`identity`) and
    /// stores the observed facts in `state`. This is the value form intended
    /// for persistence, replay, rollback planning, and cross-boot comparison.
    #[serde(rename = "snapshot")]
    Snapshot {
        kind: SnapshotKind,
        identity: String,
        state: HashMap<String, OValue>,
    },

    /// STEP-3.5: a captured but unevaluated shim invocation.
    ///
    /// Produced when a language block carries a `{lazy}` or `{defer}` attribute.
    /// Holds the same data a normal block evaluation would have built (the
    /// fully-spliced source body + the dep OValues), but with the shim NOT
    /// yet fired. The Thunk is wrapped in a Request[Eval { lang, env_id,
    /// cacheable }] so it can flow through the same orchestration as the
    /// Nix-family rung climbs.
    ///
    /// fingerprint = sha256(body || sorted dep content_identities). The
    /// lang/env_id/cacheable metadata live on the wrapping Request, not here —
    /// the Thunk is just the captured payload.
    #[serde(rename = "thunk")]
    Thunk {
        body: String,
        deps: Vec<OValue>,
        fingerprint: String,
    },

    /// A first-class group of computations carrying an explicit execution
    /// topology.
    ///
    /// A Group makes the *shape* of a multi-computation explicit in the value
    /// model. Where a Request names a single deferred computation, a Group
    /// names a collection of them together with the `mode` that says HOW they
    /// relate:
    /// - `Batch`: run all for throughput and wrap failures as `OValue::Error`.
    /// - `All`: require every member to succeed.
    /// - `Any`: yield the first member that succeeds.
    /// - `Race`: yield the first member to settle. Losers are not yet canceled.
    ///
    /// `batch`, `all`, `any`, and `race` group constructors are **special
    /// forms**: their arguments are captured as deferred Requests rather than
    /// being eagerly resolved before the group is built. Under
    /// `Policy::Autonomous`, capture preserves Autonomous policy so those
    /// deferred Requests are also buffered for the scheduler. This means
    /// `batch(realise(instantiate($e)))` always captures a Request chain —
    /// never a pre-resolved StorePath.
    ///
    /// `fingerprint = sha256("group" || mode || ordered member content identities)`.
    /// Member order is preserved (NOT sorted) because order is semantically
    /// significant: it determines the result list order for Batch/All and the
    /// preference order for Any/Race.
    ///
    /// Wire format: `{"t":"group","mode":"batch","members":[...],"fingerprint":"..."}`
    #[serde(rename = "group")]
    Group {
        mode: GroupMode,
        members: Vec<OValue>,
        fingerprint: String,
    },

    /// A captured error outcome — produced by `batch(...)` when a member fails
    /// during normal Fresh resolution. Where `all(...)` aborts the whole group
    /// on first failure, `batch(...)` continues and wraps each ordinary failure
    /// as an `OError` so the result list has exactly one entry per input member
    /// regardless of how many succeeded or failed. Strict cache misses after an
    /// autonomous flush remain hard scheduler invariant errors.
    ///
    /// `msg` is the human-readable error message from the failed computation.
    ///
    /// Wire format: `{"t":"error","msg":"..."}`
    #[serde(rename = "error")]
    Error { msg: String },
}

/// The execution topology of an `OValue::Group`.
///
/// This is the "shape" axis of coordination: given several computations, the
/// mode says how their execution relates and which results survive into the
/// resolved value. Members are dispatched concurrently for Nix-family
/// Requests; Eval Requests and plain values always resolve serially.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupMode {
    /// Run every member for throughput. Resolves to an `OList` of all member
    /// results in member order. **Ordinary Fresh-mode failures are not fatal**:
    /// a failed member is represented as an `OValue::Error` in the result list
    /// so the list always has exactly one entry per input member. Under
    /// `Policy::Autonomous`, captured Batch members are buffered and dispatched
    /// by the scheduler; a Strict-mode cache miss after flush is a hard
    /// scheduler invariant error, not an `OValue::Error`.
    Batch,

    /// Fan-out where every member must succeed. Resolves to an `OList` of all
    /// member results in member order; if **any** member fails the whole group
    /// fails immediately (hard all-or-nothing barrier). Distinguished from
    /// `Batch` by intent and by failure semantics: `all` aborts on the first
    /// error while `batch` collects every outcome.
    All,

    /// Redundancy / fallback. Resolves to the first member that succeeds,
    /// trying members left-to-right; only fails if every member fails.
    Any,

    /// Latency competition — first member to **settle** (success or failure)
    /// wins. Remaining members may still run but their results are discarded.
    ///
    /// **Note:** Race does not yet cancel losing work. Full cancellation
    /// support (cooperative tokens or subprocess kill) is a future goal.
    Race,
}

/// The broad class of authority carried by an `OValue::Capability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    File,
    MemoryRegion,
    Device,
    Clock,
    NetworkEndpoint,
    Process,
    Service,
    SystemActivation,
    BackendExecution,
}

impl CapabilityKind {
    pub fn name(&self) -> &'static str {
        match self {
            CapabilityKind::File => "file",
            CapabilityKind::MemoryRegion => "memory_region",
            CapabilityKind::Device => "device",
            CapabilityKind::Clock => "clock",
            CapabilityKind::NetworkEndpoint => "network_endpoint",
            CapabilityKind::Process => "process",
            CapabilityKind::Service => "service",
            CapabilityKind::SystemActivation => "system_activation",
            CapabilityKind::BackendExecution => "backend_execution",
        }
    }
}

/// Ambient host authority that a backend block may request explicitly.
///
/// These rights are separate from the trusted runtime's ability to start the
/// backend interpreter itself. They describe what evaluated foreign source is
/// allowed to do after the shim has started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendAuthority {
    FileRead,
    FileWrite,
    Network,
    Process,
}

impl BackendAuthority {
    pub const ALL: [Self; 4] = [
        Self::FileRead,
        Self::FileWrite,
        Self::Network,
        Self::Process,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::FileRead => "fs_read",
            Self::FileWrite => "fs_write",
            Self::Network => "network",
            Self::Process => "process",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "fs_read" => Self::FileRead,
            "fs_write" => Self::FileWrite,
            "network" => Self::Network,
            "process" => Self::Process,
            _ => return None,
        })
    }
}

/// The domain of state captured by an `OValue::Snapshot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotKind {
    System,
    Service,
    Filesystem,
    Device,
}

impl SnapshotKind {
    pub fn name(&self) -> &'static str {
        match self {
            SnapshotKind::System => "system",
            SnapshotKind::Service => "service",
            SnapshotKind::Filesystem => "filesystem",
            SnapshotKind::Device => "device",
        }
    }
}

/// Runtime boundary classification for an O value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBoundary {
    /// Pure data: serializable, replayable, and safe to persist across boots.
    Pure,
    /// A live reference into the world; stable as a handle, but not a snapshot.
    Referential,
    /// Authority-bearing or effectful control values that must be handled with
    /// extra care by schedulers, caches, and persistence layers.
    Effectful,
}

impl GroupMode {
    /// The builtin function name / wire tag that constructs this mode
    /// (`batch`, `all`, `any`, `race`). Used in fingerprint composition,
    /// error messages, and Display.
    pub fn name(&self) -> &'static str {
        match self {
            GroupMode::Batch => "batch",
            GroupMode::All => "all",
            GroupMode::Any => "any",
            GroupMode::Race => "race",
        }
    }

    /// Whether this mode yields ALL member results (`Batch`/`All`) as opposed
    /// to a single winning member (`Any`/`Race`).
    pub fn collects_all(&self) -> bool {
        matches!(self, GroupMode::Batch | GroupMode::All)
    }
}

/// The kind of computation a Request performs.
///
/// STEP4: additional kinds for OS-as-participant — e.g. `Activate` for switching
/// a NixOS configuration into the running system, `Snapshot` for capturing a
/// running machine's state, etc.
///
/// Not `Copy`: the `Eval` variant carries owned strings. Cloning is cheap
/// enough at the construction-and-dispatch frequency we operate at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    /// NixExpr → Derivation. Source must be an OValue::NixExpr (or a Request
    /// whose chained result is a NixExpr).
    Instantiate,

    /// Derivation → StorePath. Source must be an OValue::Derivation (or a
    /// Request whose chained result is a Derivation).
    Realise,

    /// STEP-3.5: fire a captured shim invocation. Source must be an
    /// OValue::Thunk (the captured body + deps).
    ///
    /// `lang` selects which backend shim runs (python, nix, html, ...).
    /// `env_id` selects the persistent env (`u32::MAX` = ephemeral for bare `lang^(...)` blocks; explicit [N] for named persistent).
    /// `cacheable` distinguishes {lazy} (true, pure backends only, force-
    /// caches by fingerprint) from {defer} (false, any backend, re-runs on
    /// every force, errors on splice).
    Eval {
        lang: String,
        env_id: u32,
        cacheable: bool,
        authority: Option<String>,
        permissions: Vec<BackendAuthority>,
    },

    /// STEP-4: activate a system closure onto a profile.
    ///
    /// Source must resolve (possibly via a chained Request[Realise]) to an
    /// OValue::StorePath — the path of a built NixOS system closure. The
    /// activation runs `<store_path>/bin/switch-to-configuration switch`,
    /// updating the profile symlink and starting/stopping services. The
    /// returned value is OValue::System pointing at the now-current
    /// generation of the profile.
    ///
    /// `profile` is the symlink path to update (e.g. `/nix/var/nix/profiles/system`).
    /// `dry_run` selects `dry-activate` instead of `switch`. Real activation is
    /// ambient host authority by default, matching what the same user could do
    /// from a shell. If `authority` is present, the evaluator treats it as an
    /// embedding-specific profile guard and validates it before the operation
    /// reaches the perform boundary.
    ///
    /// Activate is NOT cached. The cache invariant ("same fingerprint always
    /// produces the same result") is meaningless for an operation that
    /// changes world state, and a stale cached System reference would lie
    /// about the live state. The executor's cache lookup skips it.
    Activate {
        profile: String,
        dry_run: bool,
        authority: Option<String>,
    },
}

// ═════════════════════════════════════════════════════════════════════════════
// SECTION 2: Constructors
//
// Ergonomic constructors so call sites don't have to write
// OValue::Str { v: "hello".to_string() } everywhere.
// ═════════════════════════════════════════════════════════════════════════════

impl OValue {
    pub fn null() -> Self {
        OValue::Null
    }
    pub fn bool_(b: bool) -> Self {
        OValue::Bool { v: b }
    }
    pub fn int(n: i64) -> Self {
        OValue::Int { v: n }
    }
    pub fn float(f: f64) -> Self {
        OValue::Float { v: f }
    }
    pub fn number(n: ONumber) -> Self {
        OValue::Number { v: n }
    }
    pub fn big_int(n: impl Into<BigInt>) -> Self {
        OValue::Number {
            v: ONumber::Int { v: n.into() },
        }
    }
    pub fn rational(num: impl Into<BigInt>, den: impl Into<BigInt>) -> Result<Self> {
        let den = den.into();
        if den == BigInt::from(0) {
            bail!("rational denominator cannot be zero");
        }
        Ok(OValue::Number {
            v: ONumber::Rational {
                num: num.into(),
                den,
            },
        })
    }
    pub fn str_(s: impl Into<String>) -> Self {
        OValue::Str { v: s.into() }
    }
    pub fn text(s: impl Into<String>) -> Self {
        OValue::Text {
            v: OText {
                utf8: s.into(),
                encoding: Some("utf-8".to_string()),
            },
        }
    }
    pub fn text_with_encoding(s: impl Into<String>, encoding: Option<String>) -> Self {
        OValue::Text {
            v: OText {
                utf8: s.into(),
                encoding,
            },
        }
    }
    pub fn bytes(bytes: impl Into<Vec<u8>>, media_type: Option<String>) -> Self {
        OValue::Bytes {
            v: OBytes {
                bytes: bytes.into(),
                media_type,
            },
        }
    }
    pub fn char_(scalar: char) -> Self {
        OValue::Char { scalar }
    }
    pub fn html(s: impl Into<String>) -> Self {
        OValue::Html { v: s.into() }
    }
    pub fn store_path(path: impl Into<String>) -> Self {
        OValue::StorePath { path: path.into() }
    }
    pub fn list(items: Vec<OValue>) -> Self {
        OValue::List { v: items }
    }
    pub fn map(entries: HashMap<String, OValue>) -> Self {
        OValue::Map { v: entries }
    }
    pub fn seq(kind: SeqKind, items: Vec<OValue>) -> Self {
        OValue::Seq { kind, items }
    }
    pub fn tuple(items: Vec<OValue>) -> Self {
        OValue::Seq {
            kind: SeqKind::Tuple,
            items,
        }
    }
    pub fn object(fields: BTreeMap<String, OValue>) -> Self {
        OValue::Object { fields }
    }
    pub fn entries_map(entries: Vec<(OValue, OValue)>) -> Self {
        OValue::EntriesMap { entries }
    }
    pub fn set(kind: SetKind, items: Vec<OValue>) -> Self {
        OValue::Set { kind, items }
    }
    pub fn symbol(name: impl Into<String>) -> Self {
        OValue::Symbol {
            v: OSymbol {
                namespace: None,
                name: name.into(),
            },
        }
    }
    pub fn namespaced_symbol(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        OValue::Symbol {
            v: OSymbol {
                namespace: Some(namespace.into()),
                name: name.into(),
            },
        }
    }
    pub fn keyword(name: impl Into<String>) -> Self {
        OValue::Keyword {
            v: OKeyword {
                namespace: None,
                name: name.into(),
            },
        }
    }
    pub fn graph(root: NodeId, nodes: Vec<GraphNode>) -> Self {
        OValue::Graph { root, nodes }
    }
    pub fn native(native: ONative) -> Self {
        OValue::Native { v: native }
    }
    pub fn scope(bindings: HashMap<String, OValue>) -> Self {
        OValue::Scope { bindings }
    }

    /// Construct a lazy Nix expression value.
    ///
    /// `body` is the fully-spliced Nix source text.
    /// `deps` are the child OValues (by reference) whose rendered forms were
    /// spliced into `body`.
    ///
    /// STEP-2 FINGERPRINT SCHEME (the upgrade promised in step 1's annotation):
    ///   fingerprint = sha256(body || "||" || sorted(dep.content_identity()))
    ///
    /// This composes with dep identities so the cache key stays stable across
    /// rebuilds. Where deps are Nix-native content-addressed values (Derivation
    /// has drv_path, StorePath has path), we use those identities directly
    /// rather than reinventing the hash.
    pub fn nix_expr(body: impl Into<String>, deps: Vec<OValue>) -> Self {
        let body = body.into();
        let mut identities: Vec<String> = deps.iter().map(|d| d.content_identity()).collect();
        identities.sort();
        let composed = format!("{}||{}", body, identities.join("|"));
        let fingerprint = hex::encode(Sha256::digest(composed.as_bytes()));
        OValue::NixExpr {
            body,
            deps,
            fingerprint,
        }
    }

    /// Construct an instantiated derivation value.
    ///
    /// `drv_path` is the canonical content identity (assigned by Nix); no
    /// separate fingerprint is stored on the value. `outputs` enumerates the
    /// names of buildable outputs (typically just `["out"]`).
    /// `deps` is provenance — the OValues that were spliced into the source
    /// expression that produced this derivation.
    pub fn derivation(
        drv_path: impl Into<String>,
        outputs: Vec<String>,
        deps: Vec<OValue>,
    ) -> Self {
        OValue::Derivation {
            drv_path: drv_path.into(),
            outputs,
            deps,
        }
    }

    /// Construct a Request value that names a deferred computation without
    /// performing it.
    ///
    /// The fingerprint is `sha256(kind_tag || source.content_identity())`.
    /// For RequestKind::Eval the kind_tag includes the lang, env_id, and
    /// cacheable flag so requests differing in any of those (but otherwise
    /// identical) get distinct cache slots.
    pub fn request(kind: RequestKind, source: OValue) -> Self {
        let kind_tag = Self::kind_tag(&kind);
        let composed = format!("{}||{}", kind_tag, source.content_identity());
        let fingerprint = hex::encode(Sha256::digest(composed.as_bytes()));
        OValue::Request {
            kind,
            source: Box::new(source),
            fingerprint,
        }
    }

    /// String tag for a RequestKind used in fingerprint composition.
    /// Includes the data-carrying fields of the Eval variant so distinct
    /// Eval requests get distinct fingerprints.
    pub fn kind_tag(kind: &RequestKind) -> String {
        match kind {
            RequestKind::Instantiate => "instantiate".to_string(),
            RequestKind::Realise => "realise".to_string(),
            RequestKind::Eval {
                lang,
                env_id,
                cacheable,
                authority,
                permissions,
            } => {
                let permissions = permissions
                    .iter()
                    .map(|permission| permission.name())
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    "eval|{}|{}|{}|{}|{}",
                    lang,
                    env_id,
                    cacheable,
                    authority.as_deref().unwrap_or("none"),
                    permissions
                )
            }
            RequestKind::Activate {
                profile,
                dry_run,
                authority,
            } => {
                format!(
                    "activate|{}|{}|{}",
                    profile,
                    dry_run,
                    authority.as_deref().unwrap_or("none")
                )
            }
        }
    }

    /// Construct a System value pointing at a profile path. STEP-4.
    ///
    /// The profile path is the canonical identity — no hashing, no snapshot.
    /// This is intentional: a System is a reference, and two references to
    /// the same profile ARE the same System even if the underlying
    /// generation has changed between observations.
    pub fn system(profile_path: impl Into<String>) -> Self {
        OValue::System {
            profile_path: profile_path.into(),
        }
    }

    /// Construct a capability-scoped system resource handle.
    pub fn capability(
        kind: CapabilityKind,
        identity: impl Into<String>,
        metadata: HashMap<String, OValue>,
    ) -> Self {
        OValue::Capability {
            kind,
            identity: identity.into(),
            metadata,
        }
    }

    /// Construct an inert snapshot of observed world state.
    pub fn snapshot(
        kind: SnapshotKind,
        identity: impl Into<String>,
        state: HashMap<String, OValue>,
    ) -> Self {
        OValue::Snapshot {
            kind,
            identity: identity.into(),
            state,
        }
    }

    /// Construct a Thunk — the captured-but-unevaluated payload of a
    /// `{lazy}` or `{defer}` block. The Thunk is wrapped in a Request[Eval]
    /// by the caller; this constructor just builds the data carrier with
    /// the composed fingerprint.
    ///
    /// fingerprint = sha256(body || sorted(dep.content_identity())). Identical
    /// composition rule to NixExpr — same reason: a thunk that splices the
    /// same deps must have the same identity for caching to work.
    pub fn thunk(body: impl Into<String>, deps: Vec<OValue>) -> Self {
        let body = body.into();
        let mut identities: Vec<String> = deps.iter().map(|d| d.content_identity()).collect();
        identities.sort();
        let composed = format!("{}||{}", body, identities.join("|"));
        let fingerprint = hex::encode(Sha256::digest(composed.as_bytes()));
        OValue::Thunk {
            body,
            deps,
            fingerprint,
        }
    }

    /// Construct a Group — a first-class collection of computations carrying an
    /// explicit execution topology (`mode`).
    ///
    /// `members` are the (already-evaluated) OValues that make up the group,
    /// typically Requests. Member order is preserved verbatim because it is
    /// semantically significant (result ordering for Batch/All, preference
    /// ordering for Any/Race).
    ///
    /// fingerprint = sha256("group" || mode || ordered(member.content_identity())).
    /// Unlike NixExpr/Thunk, member identities are NOT sorted — two groups that
    /// differ only in member order are genuinely different computations.
    pub fn group(mode: GroupMode, members: Vec<OValue>) -> Self {
        let identities: Vec<String> = members.iter().map(|m| m.content_identity()).collect();
        let composed = format!("group|{}|{}", mode.name(), identities.join("|"));
        let fingerprint = hex::encode(Sha256::digest(composed.as_bytes()));
        OValue::Group {
            mode,
            members,
            fingerprint,
        }
    }

    /// Construct an error outcome value.
    ///
    /// Used by `batch(...)` to represent an ordinary Fresh-mode failed member as
    /// a first-class value in the result list rather than aborting the whole
    /// group. Strict cache misses after autonomous scheduling are not wrapped.
    /// `msg` is the human-readable error message from the failed computation.
    pub fn error(msg: impl Into<String>) -> Self {
        OValue::Error { msg: msg.into() }
    }

    /// The canonical content identity of an OValue.
    ///
    /// Used to compose Request fingerprints and NixExpr fingerprints. Defined
    /// uniformly so any OValue can appear as a dep without special-casing.
    ///
    /// Rules:
    /// - NixExpr uses its already composed fingerprint.
    /// - Derivation uses SHA-256 of its Nix content-addressed path.
    /// - StorePath uses SHA-256 of its path.
    /// - Request and Thunk use their construction-time fingerprints.
    /// - Other values use SHA-256 of canonical tagged bytes. `splice_repr()` is
    ///   for source injection and is not a semantic identity surface.
    pub fn content_identity(&self) -> String {
        match self {
            OValue::NixExpr { fingerprint, .. } => fingerprint.clone(),
            OValue::Thunk { fingerprint, .. } => fingerprint.clone(),
            OValue::Derivation { drv_path, .. } => hex::encode(Sha256::digest(drv_path.as_bytes())),
            OValue::StorePath { path } => hex::encode(Sha256::digest(path.as_bytes())),
            // STEP-4: a System's identity is its profile path. Note this is
            // PURELY REFERENTIAL — the same profile at two different times
            // is "the same" System for caching purposes, even though its
            // generation may differ. That's intentional: callers who care
            // about generation must query the live state explicitly.
            OValue::System { profile_path } => hex::encode(Sha256::digest(profile_path.as_bytes())),
            OValue::Capability { kind, identity, .. } => {
                let composed = format!("capability|{}|{}", kind.name(), identity);
                hex::encode(Sha256::digest(composed.as_bytes()))
            }
            OValue::Scope { bindings } => {
                let mut entries = bindings
                    .iter()
                    .map(|(name, value)| format!("{}={}", name, value.content_identity()))
                    .collect::<Vec<_>>();
                entries.sort();
                let composed = format!("scope|{}", entries.join("|"));
                hex::encode(Sha256::digest(composed.as_bytes()))
            }
            OValue::Snapshot { kind, identity, .. } => {
                let composed = format!("snapshot|{}|{}", kind.name(), identity);
                hex::encode(Sha256::digest(composed.as_bytes()))
            }
            OValue::Request { fingerprint, .. } => fingerprint.clone(),
            OValue::Group { fingerprint, .. } => fingerprint.clone(),
            other => {
                let bytes = other.canonical_bytes();
                hex::encode(Sha256::digest(bytes))
            }
        }
    }

    /// Construct an OBlob from raw bytes and a MIME type.
    /// The bytes are base64-encoded here; the wire representation stores
    /// the base64 string, not the raw bytes.
    pub fn blob(data: &[u8], mime: impl Into<String>) -> Self {
        OValue::Blob {
            v: B64.encode(data),
            mime: mime.into(),
        }
    }

    /// Decode the raw bytes from an OBlob, reversing the base64 encoding.
    /// Returns None if called on a non-Blob variant.
    pub fn blob_bytes(&self) -> Option<Vec<u8>> {
        match self {
            OValue::Blob { v, .. } => B64.decode(v).ok(),
            _ => None,
        }
    }

    /// Returns the MIME type of a Blob variant. None for all other variants.
    pub fn blob_mime(&self) -> Option<&str> {
        match self {
            OValue::Blob { mime, .. } => Some(mime.as_str()),
            _ => None,
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// SECTION 2.5: Canonical encoding
// ═════════════════════════════════════════════════════════════════════════════

pub trait CanonicalEncode {
    fn encode_canonical(&self, out: &mut Vec<u8>) -> Result<()>;
}

impl OValue {
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_canonical(&mut out)
            .expect("OValue canonical encoding is infallible");
        out
    }
}

fn canonical_tag(out: &mut Vec<u8>, tag: &str) {
    canonical_str(out, tag);
}

fn canonical_str(out: &mut Vec<u8>, s: &str) {
    canonical_bytes(out, s.as_bytes());
}

fn canonical_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    canonical_u64(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn canonical_bool(out: &mut Vec<u8>, value: bool) {
    out.push(u8::from(value));
}

fn canonical_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn canonical_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn canonical_bigint(out: &mut Vec<u8>, value: &BigInt) {
    canonical_str(out, &value.to_str_radix(10));
}

fn canonical_opt_str(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            canonical_tag(out, "some");
            canonical_str(out, value);
        }
        None => canonical_tag(out, "none"),
    }
}

fn canonical_opt_u64(out: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            canonical_tag(out, "some");
            canonical_u64(out, value);
        }
        None => canonical_tag(out, "none"),
    }
}

impl ONumber {
    fn encode_number(&self, out: &mut Vec<u8>) -> Result<()> {
        match self {
            ONumber::Int { v } => {
                canonical_tag(out, "number:int");
                canonical_bigint(out, v);
            }
            ONumber::Rational { num, den } => {
                canonical_tag(out, "number:rational");
                canonical_bigint(out, num);
                canonical_bigint(out, den);
            }
            ONumber::Decimal {
                coeff,
                exp10,
                special,
            } => {
                canonical_tag(out, "number:decimal");
                canonical_bigint(out, coeff);
                canonical_i64(out, *exp10);
                canonical_opt_str(out, special.map(decimal_special_name));
            }
            ONumber::BinaryFloat { format, bits } => {
                canonical_tag(out, "number:binary_float");
                canonical_tag(out, float_format_name(*format));
                canonical_bytes(out, bits);
            }
            ONumber::BigFloat {
                mantissa,
                exp2,
                precision,
                special,
            } => {
                canonical_tag(out, "number:big_float");
                canonical_bigint(out, mantissa);
                canonical_i64(out, *exp2);
                canonical_opt_u64(out, *precision);
                canonical_opt_str(out, special.map(float_special_name));
            }
            ONumber::Complex { re, im } => {
                canonical_tag(out, "number:complex");
                re.encode_number(out)?;
                im.encode_number(out)?;
            }
        }
        Ok(())
    }
}

impl CanonicalEncode for OValue {
    fn encode_canonical(&self, out: &mut Vec<u8>) -> Result<()> {
        canonical_tag(out, "ovalue");
        match self {
            OValue::Null => canonical_tag(out, "null"),
            OValue::Bool { v } => {
                canonical_tag(out, "bool");
                canonical_bool(out, *v);
            }
            OValue::Int { v } => {
                canonical_tag(out, "int");
                canonical_i64(out, *v);
            }
            OValue::Float { v } => {
                canonical_tag(out, "float");
                out.extend_from_slice(&v.to_bits().to_be_bytes());
            }
            OValue::Number { v } => v.encode_number(out)?,
            OValue::Str { v } => {
                canonical_tag(out, "str");
                canonical_str(out, v);
            }
            OValue::Text { v } => {
                canonical_tag(out, "text");
                canonical_str(out, &v.utf8);
                canonical_opt_str(out, v.encoding.as_deref());
            }
            OValue::Char { scalar } => {
                canonical_tag(out, "char");
                canonical_u64(out, *scalar as u32 as u64);
            }
            OValue::Html { v } => {
                canonical_tag(out, "html");
                canonical_str(out, v);
            }
            OValue::StorePath { path } => {
                canonical_tag(out, "store_path");
                canonical_str(out, path);
            }
            OValue::Expr { src } => {
                canonical_tag(out, "expr");
                canonical_str(out, src);
            }
            OValue::List { v } => {
                canonical_tag(out, "list");
                canonical_u64(out, v.len() as u64);
                for value in v {
                    value.encode_canonical(out)?;
                }
            }
            OValue::Map { v } => {
                canonical_tag(out, "map");
                let mut entries = v.iter().collect::<Vec<_>>();
                entries.sort_by_key(|(key, _)| *key);
                canonical_u64(out, entries.len() as u64);
                for (key, value) in entries {
                    canonical_str(out, key);
                    value.encode_canonical(out)?;
                }
            }
            OValue::Seq { kind, items } => {
                canonical_tag(out, "seq");
                canonical_tag(out, seq_kind_name(*kind));
                canonical_u64(out, items.len() as u64);
                for value in items {
                    value.encode_canonical(out)?;
                }
            }
            OValue::Object { fields } => {
                canonical_tag(out, "object");
                canonical_u64(out, fields.len() as u64);
                for (key, value) in fields {
                    canonical_str(out, key);
                    value.encode_canonical(out)?;
                }
            }
            OValue::EntriesMap { entries } => {
                canonical_tag(out, "entries_map");
                let mut encoded_entries = entries
                    .iter()
                    .map(|(key, value)| {
                        let key_bytes = key.canonical_bytes();
                        let value_bytes = value.canonical_bytes();
                        (key_bytes, value_bytes)
                    })
                    .collect::<Vec<_>>();
                encoded_entries.sort();
                canonical_u64(out, encoded_entries.len() as u64);
                for (key_bytes, value_bytes) in encoded_entries {
                    canonical_bytes(out, &key_bytes);
                    canonical_bytes(out, &value_bytes);
                }
            }
            OValue::Set { kind, items } => {
                canonical_tag(out, "set");
                canonical_tag(out, set_kind_name(*kind));
                let mut encoded_items = items
                    .iter()
                    .map(OValue::canonical_bytes)
                    .collect::<Vec<_>>();
                if matches!(kind, SetKind::Unordered) {
                    encoded_items.sort();
                }
                canonical_u64(out, encoded_items.len() as u64);
                for item in encoded_items {
                    canonical_bytes(out, &item);
                }
            }
            OValue::Symbol { v } => {
                canonical_tag(out, "symbol");
                canonical_opt_str(out, v.namespace.as_deref());
                canonical_str(out, &v.name);
            }
            OValue::Keyword { v } => {
                canonical_tag(out, "keyword");
                canonical_opt_str(out, v.namespace.as_deref());
                canonical_str(out, &v.name);
            }
            OValue::Scope { bindings } => {
                canonical_tag(out, "scope");
                let mut entries = bindings.iter().collect::<Vec<_>>();
                entries.sort_by_key(|(key, _)| *key);
                canonical_u64(out, entries.len() as u64);
                for (key, value) in entries {
                    canonical_str(out, key);
                    value.encode_canonical(out)?;
                }
            }
            OValue::Blob { v, mime } => {
                canonical_tag(out, "blob");
                canonical_str(out, mime);
                match B64.decode(v) {
                    Ok(bytes) => {
                        canonical_tag(out, "base64:decoded");
                        canonical_bytes(out, &bytes);
                    }
                    Err(_) => {
                        canonical_tag(out, "base64:invalid");
                        canonical_str(out, v);
                    }
                }
            }
            OValue::Bytes { v } => {
                canonical_tag(out, "bytes");
                canonical_opt_str(out, v.media_type.as_deref());
                canonical_bytes(out, &v.bytes);
            }
            OValue::Graph { root, nodes } => {
                canonical_tag(out, "graph");
                canonical_u64(out, *root);
                canonical_u64(out, nodes.len() as u64);
                for node in nodes {
                    node.encode_canonical(out)?;
                }
            }
            OValue::Native { v } => v.encode_canonical(out)?,
            OValue::NixExpr {
                body,
                deps,
                fingerprint,
            } => {
                canonical_tag(out, "nix_expr");
                canonical_str(out, body);
                canonical_u64(out, deps.len() as u64);
                for dep in deps {
                    dep.encode_canonical(out)?;
                }
                canonical_str(out, fingerprint);
            }
            OValue::Derivation {
                drv_path,
                outputs,
                deps,
            } => {
                canonical_tag(out, "derivation");
                canonical_str(out, drv_path);
                canonical_u64(out, outputs.len() as u64);
                for output in outputs {
                    canonical_str(out, output);
                }
                canonical_u64(out, deps.len() as u64);
                for dep in deps {
                    dep.encode_canonical(out)?;
                }
            }
            OValue::Request {
                kind,
                source,
                fingerprint,
            } => {
                canonical_tag(out, "request");
                kind.encode_canonical(out)?;
                source.encode_canonical(out)?;
                canonical_str(out, fingerprint);
            }
            OValue::System { profile_path } => {
                canonical_tag(out, "system");
                canonical_str(out, profile_path);
            }
            OValue::Capability {
                kind,
                identity,
                metadata,
            } => {
                canonical_tag(out, "capability");
                canonical_tag(out, kind.name());
                canonical_str(out, identity);
                let mut entries = metadata.iter().collect::<Vec<_>>();
                entries.sort_by_key(|(key, _)| *key);
                canonical_u64(out, entries.len() as u64);
                for (key, value) in entries {
                    canonical_str(out, key);
                    value.encode_canonical(out)?;
                }
            }
            OValue::Snapshot {
                kind,
                identity,
                state,
            } => {
                canonical_tag(out, "snapshot");
                canonical_tag(out, kind.name());
                canonical_str(out, identity);
                let mut entries = state.iter().collect::<Vec<_>>();
                entries.sort_by_key(|(key, _)| *key);
                canonical_u64(out, entries.len() as u64);
                for (key, value) in entries {
                    canonical_str(out, key);
                    value.encode_canonical(out)?;
                }
            }
            OValue::Thunk {
                body,
                deps,
                fingerprint,
            } => {
                canonical_tag(out, "thunk");
                canonical_str(out, body);
                canonical_u64(out, deps.len() as u64);
                for dep in deps {
                    dep.encode_canonical(out)?;
                }
                canonical_str(out, fingerprint);
            }
            OValue::Group {
                mode,
                members,
                fingerprint,
            } => {
                canonical_tag(out, "group");
                canonical_tag(out, mode.name());
                canonical_u64(out, members.len() as u64);
                for member in members {
                    member.encode_canonical(out)?;
                }
                canonical_str(out, fingerprint);
            }
            OValue::Error { msg } => {
                canonical_tag(out, "error");
                canonical_str(out, msg);
            }
        }
        Ok(())
    }
}

impl CanonicalEncode for GraphNode {
    fn encode_canonical(&self, out: &mut Vec<u8>) -> Result<()> {
        match self {
            GraphNode::Value { value } => {
                canonical_tag(out, "graph_node:value");
                value.encode_canonical(out)?;
            }
            GraphNode::Ref { target } => {
                canonical_tag(out, "graph_node:ref");
                canonical_u64(out, *target);
            }
        }
        Ok(())
    }
}

impl CanonicalEncode for ONative {
    fn encode_canonical(&self, out: &mut Vec<u8>) -> Result<()> {
        canonical_tag(out, "native");
        canonical_str(out, &self.lang);
        canonical_opt_str(out, self.implementation.as_deref());
        canonical_opt_str(out, self.version.as_deref());
        canonical_str(out, &self.type_name);
        canonical_opt_str(out, self.identity.stable.as_deref());
        canonical_opt_str(out, self.identity.live.as_deref());
        canonical_str(out, &self.codec);
        match &self.payload {
            Some(payload) => {
                canonical_tag(out, "some");
                canonical_opt_str(out, payload.media_type.as_deref());
                canonical_bytes(out, &payload.bytes);
            }
            None => canonical_tag(out, "none"),
        }
        canonical_tag(out, native_boundary_name(self.boundary));
        canonical_tag(out, native_codec_safety_name(self.safety));
        let mut capabilities = self.capabilities.clone();
        capabilities.sort_by_key(|kind| kind.name());
        canonical_u64(out, capabilities.len() as u64);
        for capability in capabilities {
            canonical_tag(out, capability.name());
        }
        canonical_u64(out, self.metadata.len() as u64);
        for (key, value) in &self.metadata {
            canonical_str(out, key);
            value.encode_canonical(out)?;
        }
        canonical_tag(out, rehydrate_policy_name(self.rehydrate));
        Ok(())
    }
}

impl CanonicalEncode for RequestKind {
    fn encode_canonical(&self, out: &mut Vec<u8>) -> Result<()> {
        match self {
            RequestKind::Instantiate => canonical_tag(out, "request_kind:instantiate"),
            RequestKind::Realise => canonical_tag(out, "request_kind:realise"),
            RequestKind::Eval {
                lang,
                env_id,
                cacheable,
                authority,
                permissions,
            } => {
                canonical_tag(out, "request_kind:eval");
                canonical_str(out, lang);
                canonical_u64(out, u64::from(*env_id));
                canonical_bool(out, *cacheable);
                canonical_opt_str(out, authority.as_deref());
                canonical_u64(out, permissions.len() as u64);
                for permission in permissions {
                    canonical_tag(out, permission.name());
                }
            }
            RequestKind::Activate {
                profile,
                dry_run,
                authority,
            } => {
                canonical_tag(out, "request_kind:activate");
                canonical_str(out, profile);
                canonical_bool(out, *dry_run);
                canonical_opt_str(out, authority.as_deref());
            }
        }
        Ok(())
    }
}

fn decimal_special_name(value: DecimalSpecial) -> &'static str {
    match value {
        DecimalSpecial::Nan => "nan",
        DecimalSpecial::PosInf => "pos_inf",
        DecimalSpecial::NegInf => "neg_inf",
        DecimalSpecial::PosZero => "pos_zero",
        DecimalSpecial::NegZero => "neg_zero",
    }
}

fn float_format_name(value: FloatFormat) -> &'static str {
    match value {
        FloatFormat::F32 => "f32",
        FloatFormat::F64 => "f64",
    }
}

fn float_special_name(value: FloatSpecial) -> &'static str {
    match value {
        FloatSpecial::Nan => "nan",
        FloatSpecial::PosInf => "pos_inf",
        FloatSpecial::NegInf => "neg_inf",
        FloatSpecial::PosZero => "pos_zero",
        FloatSpecial::NegZero => "neg_zero",
    }
}

fn seq_kind_name(value: SeqKind) -> &'static str {
    match value {
        SeqKind::List => "list",
        SeqKind::Tuple => "tuple",
        SeqKind::Vector => "vector",
    }
}

fn set_kind_name(value: SetKind) -> &'static str {
    match value {
        SetKind::Ordered => "ordered",
        SetKind::Unordered => "unordered",
    }
}

fn native_codec_safety_name(value: NativeCodecSafety) -> &'static str {
    match value {
        NativeCodecSafety::DataOnly => "data_only",
        NativeCodecSafety::SourceBacked => "source_backed",
        NativeCodecSafety::UnsafeOpaque => "unsafe_opaque",
        NativeCodecSafety::LiveHandle => "live_handle",
    }
}

fn native_boundary_name(value: NativeBoundary) -> &'static str {
    match value {
        NativeBoundary::Pure => "pure",
        NativeBoundary::Referential => "referential",
        NativeBoundary::Effectful => "effectful",
    }
}

fn rehydrate_policy_name(value: RehydratePolicy) -> &'static str {
    match value {
        RehydratePolicy::Portable => "portable",
        RehydratePolicy::SameBackend => "same_backend",
        RehydratePolicy::SameProcess => "same_process",
        RehydratePolicy::Never => "never",
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// SECTION 3: Type predicates
// ═════════════════════════════════════════════════════════════════════════════

impl OValue {
    pub fn is_null(&self) -> bool {
        matches!(self, OValue::Null)
    }
    pub fn is_bool(&self) -> bool {
        matches!(self, OValue::Bool { .. })
    }
    pub fn is_int(&self) -> bool {
        matches!(
            self,
            OValue::Int { .. }
                | OValue::Number {
                    v: ONumber::Int { .. }
                }
        )
    }
    pub fn is_float(&self) -> bool {
        matches!(self, OValue::Float { .. })
    }
    pub fn is_number(&self) -> bool {
        matches!(self, OValue::Number { .. })
    }
    pub fn is_str(&self) -> bool {
        matches!(self, OValue::Str { .. })
    }
    pub fn is_text(&self) -> bool {
        matches!(self, OValue::Text { .. })
    }
    pub fn is_bytes(&self) -> bool {
        matches!(self, OValue::Bytes { .. })
    }
    pub fn is_html(&self) -> bool {
        matches!(self, OValue::Html { .. })
    }
    pub fn is_store_path(&self) -> bool {
        matches!(self, OValue::StorePath { .. })
    }
    pub fn is_list(&self) -> bool {
        matches!(self, OValue::List { .. })
    }
    pub fn is_map(&self) -> bool {
        matches!(self, OValue::Map { .. })
    }
    pub fn is_seq(&self) -> bool {
        matches!(self, OValue::Seq { .. })
    }
    pub fn is_object(&self) -> bool {
        matches!(self, OValue::Object { .. })
    }
    pub fn is_entries_map(&self) -> bool {
        matches!(self, OValue::EntriesMap { .. })
    }
    pub fn is_set(&self) -> bool {
        matches!(self, OValue::Set { .. })
    }
    pub fn is_symbol(&self) -> bool {
        matches!(self, OValue::Symbol { .. })
    }
    pub fn is_keyword(&self) -> bool {
        matches!(self, OValue::Keyword { .. })
    }
    pub fn is_scope(&self) -> bool {
        matches!(self, OValue::Scope { .. })
    }
    pub fn is_blob(&self) -> bool {
        matches!(self, OValue::Blob { .. })
    }
    pub fn is_graph(&self) -> bool {
        matches!(self, OValue::Graph { .. })
    }
    pub fn is_native(&self) -> bool {
        matches!(self, OValue::Native { .. })
    }
    pub fn is_nix_expr(&self) -> bool {
        matches!(self, OValue::NixExpr { .. })
    }
    pub fn is_derivation(&self) -> bool {
        matches!(self, OValue::Derivation { .. })
    }
    pub fn is_request(&self) -> bool {
        matches!(self, OValue::Request { .. })
    }
    pub fn is_thunk(&self) -> bool {
        matches!(self, OValue::Thunk { .. })
    }
    pub fn is_group(&self) -> bool {
        matches!(self, OValue::Group { .. })
    }
    pub fn is_system(&self) -> bool {
        matches!(self, OValue::System { .. })
    }
    pub fn is_capability(&self) -> bool {
        matches!(self, OValue::Capability { .. })
    }
    pub fn is_snapshot(&self) -> bool {
        matches!(self, OValue::Snapshot { .. })
    }
    pub fn is_expr(&self) -> bool {
        matches!(self, OValue::Expr { .. })
    }
    pub fn is_error(&self) -> bool {
        matches!(self, OValue::Error { .. })
    }
    pub fn is_numeric(&self) -> bool {
        self.is_int() || self.is_float() || self.is_number()
    }

    /// The type name as it appears in the wire protocol `t` field.
    pub fn type_name(&self) -> &'static str {
        match self {
            OValue::Null => "null",
            OValue::Bool { .. } => "bool",
            OValue::Int { .. } => "int",
            OValue::Float { .. } => "float",
            OValue::Number { .. } => "number",
            OValue::Str { .. } => "str",
            OValue::Text { .. } => "text",
            OValue::Bytes { .. } => "bytes",
            OValue::Char { .. } => "char",
            OValue::Html { .. } => "html",
            OValue::StorePath { .. } => "store_path",
            OValue::List { .. } => "list",
            OValue::Map { .. } => "map",
            OValue::Seq { .. } => "seq",
            OValue::Object { .. } => "object",
            OValue::EntriesMap { .. } => "entries_map",
            OValue::Set { .. } => "set",
            OValue::Symbol { .. } => "symbol",
            OValue::Keyword { .. } => "keyword",
            OValue::Scope { .. } => "scope",
            OValue::Blob { .. } => "blob",
            OValue::Graph { .. } => "graph",
            OValue::Native { .. } => "native",
            OValue::NixExpr { .. } => "nix_expr",
            OValue::Derivation { .. } => "derivation",
            OValue::Request { .. } => "request",
            OValue::Thunk { .. } => "thunk",
            OValue::Group { .. } => "group",
            OValue::System { .. } => "system",
            OValue::Capability { .. } => "capability",
            OValue::Snapshot { .. } => "snapshot",
            OValue::Expr { .. } => "expr",
            OValue::Error { .. } => "error",
        }
    }

    /// How this value crosses the runtime / world boundary.
    pub fn runtime_boundary(&self) -> RuntimeBoundary {
        match self {
            OValue::System { .. } => RuntimeBoundary::Referential,
            OValue::Capability { .. }
            | OValue::Scope { .. }
            | OValue::Request { .. }
            | OValue::Group { .. }
            | OValue::Error { .. } => RuntimeBoundary::Effectful,
            OValue::Native { v } => match v.boundary {
                NativeBoundary::Pure => RuntimeBoundary::Pure,
                NativeBoundary::Referential => RuntimeBoundary::Referential,
                NativeBoundary::Effectful => RuntimeBoundary::Effectful,
            },
            OValue::List { v } | OValue::Seq { items: v, .. } | OValue::Set { items: v, .. } => {
                Self::join_child_boundaries(v.iter().map(OValue::runtime_boundary))
            }
            OValue::Map { v } => {
                Self::join_child_boundaries(v.values().map(OValue::runtime_boundary))
            }
            OValue::Object { fields } => {
                Self::join_child_boundaries(fields.values().map(OValue::runtime_boundary))
            }
            OValue::EntriesMap { entries } => Self::join_child_boundaries(
                entries
                    .iter()
                    .flat_map(|(key, value)| [key.runtime_boundary(), value.runtime_boundary()]),
            ),
            OValue::Graph { nodes, .. } => {
                Self::join_child_boundaries(nodes.iter().filter_map(|node| match node {
                    GraphNode::Value { value } => Some(value.runtime_boundary()),
                    GraphNode::Ref { .. } => None,
                }))
            }
            OValue::NixExpr { deps, .. }
            | OValue::Derivation { deps, .. }
            | OValue::Thunk { deps, .. } => {
                Self::join_child_boundaries(deps.iter().map(OValue::runtime_boundary))
            }
            OValue::Snapshot { state, .. } => {
                Self::join_child_boundaries(state.values().map(OValue::runtime_boundary))
            }
            _ => RuntimeBoundary::Pure,
        }
    }

    /// Whether this value can be encoded and shipped across process boundaries.
    ///
    /// Today every `OValue` is serializable via the hosted wire schema.
    pub fn is_serializable(&self) -> bool {
        true
    }

    /// Whether this value is safe to reuse from a cache without consulting the
    /// live world again.
    pub fn is_cache_safe(&self) -> bool {
        match self {
            OValue::System { .. } | OValue::Capability { .. } | OValue::Scope { .. } => false,
            OValue::Request {
                kind: RequestKind::Activate { .. },
                ..
            } => false,
            OValue::Request {
                kind:
                    RequestKind::Eval {
                        cacheable,
                        authority,
                        permissions,
                        ..
                    },
                source,
                ..
            } => {
                *cacheable
                    && authority.is_none()
                    && permissions.is_empty()
                    && source.is_cache_safe()
            }
            OValue::Request { source, .. } => source.is_cache_safe(),
            OValue::List { v } | OValue::Seq { items: v, .. } | OValue::Set { items: v, .. } => {
                v.iter().all(OValue::is_cache_safe)
            }
            OValue::Map { v } => v.values().all(OValue::is_cache_safe),
            OValue::Object { fields } => fields.values().all(OValue::is_cache_safe),
            OValue::EntriesMap { entries } => entries
                .iter()
                .all(|(key, value)| key.is_cache_safe() && value.is_cache_safe()),
            OValue::Graph { nodes, .. } => nodes.iter().all(|node| match node {
                GraphNode::Value { value } => value.is_cache_safe(),
                GraphNode::Ref { .. } => true,
            }),
            OValue::NixExpr { deps, .. }
            | OValue::Derivation { deps, .. }
            | OValue::Thunk { deps, .. } => deps.iter().all(OValue::is_cache_safe),
            OValue::Snapshot { state, .. } => state.values().all(OValue::is_cache_safe),
            OValue::Native { v } => Self::native_is_cache_safe(v),
            OValue::Error { .. } => false,
            _ => true,
        }
    }

    /// Whether replaying this value across time preserves its meaning.
    pub fn is_replay_safe(&self) -> bool {
        match self {
            OValue::System { .. }
            | OValue::Capability { .. }
            | OValue::Scope { .. }
            | OValue::Request {
                kind: RequestKind::Activate { .. },
                ..
            } => false,
            OValue::Request {
                kind:
                    RequestKind::Eval {
                        authority,
                        permissions,
                        ..
                    },
                source,
                ..
            } => authority.is_none() && permissions.is_empty() && source.is_replay_safe(),
            OValue::Request { source, .. } => source.is_replay_safe(),
            OValue::List { v } | OValue::Seq { items: v, .. } | OValue::Set { items: v, .. } => {
                v.iter().all(OValue::is_replay_safe)
            }
            OValue::Map { v } => v.values().all(OValue::is_replay_safe),
            OValue::Object { fields } => fields.values().all(OValue::is_replay_safe),
            OValue::EntriesMap { entries } => entries
                .iter()
                .all(|(key, value)| key.is_replay_safe() && value.is_replay_safe()),
            OValue::Graph { nodes, .. } => nodes.iter().all(|node| match node {
                GraphNode::Value { value } => value.is_replay_safe(),
                GraphNode::Ref { .. } => true,
            }),
            OValue::NixExpr { deps, .. }
            | OValue::Derivation { deps, .. }
            | OValue::Thunk { deps, .. } => deps.iter().all(OValue::is_replay_safe),
            OValue::Snapshot { state, .. } => state.values().all(OValue::is_replay_safe),
            OValue::Native { v } => Self::native_is_replay_safe(v),
            OValue::Error { .. } => false,
            _ => true,
        }
    }

    /// Whether this value is safe to persist across boots as an inert artifact.
    pub fn is_boot_persistable(&self) -> bool {
        match self {
            OValue::System { .. }
            | OValue::Capability { .. }
            | OValue::Scope { .. }
            | OValue::Request {
                kind: RequestKind::Activate { .. },
                ..
            } => false,
            OValue::Request {
                kind:
                    RequestKind::Eval {
                        authority,
                        permissions,
                        ..
                    },
                source,
                ..
            } => authority.is_none() && permissions.is_empty() && source.is_boot_persistable(),
            OValue::Request { source, .. } => source.is_boot_persistable(),
            OValue::List { v } | OValue::Seq { items: v, .. } | OValue::Set { items: v, .. } => {
                v.iter().all(OValue::is_boot_persistable)
            }
            OValue::Map { v } => v.values().all(OValue::is_boot_persistable),
            OValue::Object { fields } => fields.values().all(OValue::is_boot_persistable),
            OValue::EntriesMap { entries } => entries
                .iter()
                .all(|(key, value)| key.is_boot_persistable() && value.is_boot_persistable()),
            OValue::Graph { nodes, .. } => nodes.iter().all(|node| match node {
                GraphNode::Value { value } => value.is_boot_persistable(),
                GraphNode::Ref { .. } => true,
            }),
            OValue::NixExpr { deps, .. }
            | OValue::Derivation { deps, .. }
            | OValue::Thunk { deps, .. } => deps.iter().all(OValue::is_boot_persistable),
            OValue::Snapshot { state, .. } => state.values().all(OValue::is_boot_persistable),
            OValue::Native { v } => Self::native_is_boot_persistable(v),
            OValue::Error { .. } => false,
            _ => true,
        }
    }

    fn join_child_boundaries(boundaries: impl Iterator<Item = RuntimeBoundary>) -> RuntimeBoundary {
        boundaries.fold(RuntimeBoundary::Pure, |acc, boundary| {
            match (acc, boundary) {
                (RuntimeBoundary::Effectful, _) | (_, RuntimeBoundary::Effectful) => {
                    RuntimeBoundary::Effectful
                }
                (RuntimeBoundary::Referential, _) | (_, RuntimeBoundary::Referential) => {
                    RuntimeBoundary::Referential
                }
                _ => RuntimeBoundary::Pure,
            }
        })
    }

    fn native_is_cache_safe(native: &ONative) -> bool {
        matches!(
            native.safety,
            NativeCodecSafety::DataOnly | NativeCodecSafety::SourceBacked
        ) && native.boundary == NativeBoundary::Pure
            && native.identity.live.is_none()
            && native.capabilities.is_empty()
            && native.metadata.values().all(OValue::is_cache_safe)
    }

    fn native_is_replay_safe(native: &ONative) -> bool {
        matches!(
            native.safety,
            NativeCodecSafety::DataOnly | NativeCodecSafety::SourceBacked
        ) && native.boundary == NativeBoundary::Pure
            && native.identity.live.is_none()
            && native.capabilities.is_empty()
            && native.metadata.values().all(OValue::is_replay_safe)
    }

    fn native_is_boot_persistable(native: &ONative) -> bool {
        matches!(
            native.safety,
            NativeCodecSafety::DataOnly | NativeCodecSafety::SourceBacked
        ) && native.boundary == NativeBoundary::Pure
            && native.rehydrate == RehydratePolicy::Portable
            && native.identity.live.is_none()
            && native.capabilities.is_empty()
            && native.metadata.values().all(OValue::is_boot_persistable)
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// SECTION 4: Coercions
//
// Safe, explicit coercions from OValue to Rust native types.
// These never panic — they return Result so the caller handles mismatches.
// The O evaluator uses these when splicing values into backend code strings.
// ═════════════════════════════════════════════════════════════════════════════

impl OValue {
    pub fn as_bool(&self) -> Result<bool> {
        match self {
            OValue::Bool { v } => Ok(*v),
            other => bail!("Expected bool, got {}", other.type_name()),
        }
    }

    pub fn as_int(&self) -> Result<i64> {
        match self {
            OValue::Int { v } => Ok(*v),
            OValue::Number {
                v: ONumber::Int { v },
            } => v
                .to_i64()
                .ok_or_else(|| anyhow::anyhow!("Expected i64-sized int, got number")),
            other => bail!("Expected int, got {}", other.type_name()),
        }
    }

    pub fn as_float(&self) -> Result<f64> {
        match self {
            OValue::Float { v } => Ok(*v),
            // Implicit int → float widening, because this is always safe
            OValue::Int { v } => Ok(*v as f64),
            OValue::Number {
                v: ONumber::Int { v },
            } => v
                .to_f64()
                .ok_or_else(|| anyhow::anyhow!("Expected float-sized number, got big int")),
            OValue::Number {
                v: ONumber::Rational { num, den },
            } => {
                let num = num
                    .to_f64()
                    .ok_or_else(|| anyhow::anyhow!("Expected float-sized rational numerator"))?;
                let den = den
                    .to_f64()
                    .ok_or_else(|| anyhow::anyhow!("Expected float-sized rational denominator"))?;
                Ok(num / den)
            }
            OValue::Number {
                v:
                    ONumber::BinaryFloat {
                        format: FloatFormat::F32,
                        bits,
                    },
            } if bits.len() == 4 => {
                let mut raw = [0_u8; 4];
                raw.copy_from_slice(bits);
                Ok(f32::from_bits(u32::from_be_bytes(raw)) as f64)
            }
            OValue::Number {
                v:
                    ONumber::BinaryFloat {
                        format: FloatFormat::F64,
                        bits,
                    },
            } if bits.len() == 8 => {
                let mut raw = [0_u8; 8];
                raw.copy_from_slice(bits);
                Ok(f64::from_bits(u64::from_be_bytes(raw)))
            }
            other => bail!("Expected float, got {}", other.type_name()),
        }
    }

    pub fn as_str(&self) -> Result<&str> {
        match self {
            OValue::Str { v } => Ok(v.as_str()),
            OValue::Text { v } => Ok(v.utf8.as_str()),
            other => bail!("Expected str, got {}", other.type_name()),
        }
    }

    pub fn as_list(&self) -> Result<&Vec<OValue>> {
        match self {
            OValue::List { v } => Ok(v),
            OValue::Seq { items, .. } => Ok(items),
            other => bail!("Expected list, got {}", other.type_name()),
        }
    }

    pub fn as_map(&self) -> Result<&HashMap<String, OValue>> {
        match self {
            OValue::Map { v } => Ok(v),
            other => bail!("Expected map, got {}", other.type_name()),
        }
    }
}

fn number_repr(value: &ONumber) -> String {
    match value {
        ONumber::Int { v } => v.to_string(),
        ONumber::Rational { num, den } => format!("{}/{}", num, den),
        ONumber::Decimal {
            coeff,
            exp10,
            special,
        } => match special {
            Some(special) => decimal_special_name(*special).to_string(),
            None => format!("{}e{}", coeff, exp10),
        },
        ONumber::BinaryFloat {
            format: FloatFormat::F32,
            bits,
        } if bits.len() == 4 => {
            let mut raw = [0_u8; 4];
            raw.copy_from_slice(bits);
            f32::from_bits(u32::from_be_bytes(raw)).to_string()
        }
        ONumber::BinaryFloat {
            format: FloatFormat::F64,
            bits,
        } if bits.len() == 8 => {
            let mut raw = [0_u8; 8];
            raw.copy_from_slice(bits);
            f64::from_bits(u64::from_be_bytes(raw)).to_string()
        }
        ONumber::BinaryFloat { format, bits } => {
            format!("<{}:{}>", float_format_name(*format), hex::encode(bits))
        }
        ONumber::BigFloat {
            mantissa,
            exp2,
            precision,
            special,
        } => match special {
            Some(special) => float_special_name(*special).to_string(),
            None => match precision {
                Some(precision) => format!("{}p{}:{}", mantissa, exp2, precision),
                None => format!("{}p{}", mantissa, exp2),
            },
        },
        ONumber::Complex { re, im } => format!("{}+{}i", number_repr(re), number_repr(im)),
    }
}

fn symbol_repr(value: &OSymbol) -> String {
    match &value.namespace {
        Some(namespace) => format!("{}/{}", namespace, value.name),
        None => value.name.clone(),
    }
}

fn keyword_repr(value: &OKeyword) -> String {
    match &value.namespace {
        Some(namespace) => format!(":{}/{}", namespace, value.name),
        None => format!(":{}", value.name),
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// SECTION 5: Splice representation
//
// When an OValue is used as an atom inside a backend's code string — the
// result of evaluating a nested typed expression — it must be converted to
// a string that is syntactically valid in the receiving language.
//
// This is the `$var` splice operation. The representation is conservative:
// it favors forms that are valid in the widest range of languages.
//
// OBlob is special: it splices as a data URI (base64 inline), which is
// valid in HTML, CSS, and as a Python bytes literal prefix.
// ═════════════════════════════════════════════════════════════════════════════

impl OValue {
    /// Convert to a string suitable for splicing into backend source code.
    /// This is what `$var` resolves to when the variable's value is substituted
    /// into the surrounding expression's code string.
    pub fn splice_repr(&self) -> String {
        match self {
            OValue::Null => "null".to_string(),
            OValue::Bool { v } => v.to_string(),
            OValue::Int { v } => v.to_string(),
            OValue::Float { v } => {
                // Always include decimal point — "3" vs "3.0" matters in some langs
                if v.fract() == 0.0 {
                    format!("{:.1}", v)
                } else {
                    v.to_string()
                }
            }
            OValue::Number { v } => number_repr(v),
            OValue::Str { v } => v.clone(),
            OValue::Text { v } => v.utf8.clone(),
            OValue::Char { scalar } => scalar.to_string(),
            OValue::Html { v } => v.clone(),
            OValue::StorePath { path } => path.clone(),
            OValue::List { v } => {
                let items: Vec<String> = v.iter().map(|i| i.splice_repr()).collect();
                format!("[{}]", items.join(", "))
            }
            OValue::Map { v } => {
                let mut entries = v.iter().collect::<Vec<_>>();
                entries.sort_by_key(|(key, _)| *key);
                let pairs: Vec<String> = entries
                    .into_iter()
                    .map(|(k, val)| format!("{:?}: {}", k, val.splice_repr()))
                    .collect();
                format!("{{{}}}", pairs.join(", "))
            }
            OValue::Seq { kind, items } => {
                let items: Vec<String> = items.iter().map(|i| i.splice_repr()).collect();
                match kind {
                    SeqKind::Tuple => format!("({})", items.join(", ")),
                    SeqKind::List | SeqKind::Vector => format!("[{}]", items.join(", ")),
                }
            }
            OValue::Object { fields } => {
                let pairs: Vec<String> = fields
                    .iter()
                    .map(|(k, val)| format!("{:?}: {}", k, val.splice_repr()))
                    .collect();
                format!("{{{}}}", pairs.join(", "))
            }
            OValue::EntriesMap { entries } => {
                let pairs: Vec<String> = entries
                    .iter()
                    .map(|(k, val)| format!("{} => {}", k.splice_repr(), val.splice_repr()))
                    .collect();
                format!("{{{}}}", pairs.join(", "))
            }
            OValue::Set { items, .. } => {
                let items: Vec<String> = items.iter().map(|i| i.splice_repr()).collect();
                format!("{{{}}}", items.join(", "))
            }
            OValue::Symbol { v } => symbol_repr(v),
            OValue::Keyword { v } => keyword_repr(v),
            OValue::Scope { bindings } => format!("<scope bindings={}>", bindings.len()),
            OValue::Blob { v, mime } => {
                format!("data:{};base64,{}", mime, v)
            }
            OValue::Bytes { v } => match &v.media_type {
                Some(media_type) => format!("data:{};base64,{}", media_type, B64.encode(&v.bytes)),
                None => format!("<bytes:{}>", v.bytes.len()),
            },
            OValue::Graph { root, nodes } => {
                format!("<graph root={} nodes={}>", root, nodes.len())
            }
            OValue::Native { v } => {
                format!("<native:{} {}>", v.lang, v.type_name)
            }
            // ONixExpr splices as the raw Nix body — the expression is already
            // valid Nix source text that can be embedded directly in a Nix context.
            OValue::NixExpr { body, .. } => body.clone(),

            // A Derivation splices as its .drv path. In a Nix context this is
            // also a valid path expression. In other languages it is just a
            // string identifier — the receiving backend can decide.
            OValue::Derivation { drv_path, .. } => drv_path.clone(),

            // A Request has no natural splice form — it is a control value,
            // not a data value. We splice its fingerprint as a marker; the
            // evaluator's splice loop intercepts {lazy} Eval requests and
            // auto-forces them BEFORE rendering (per the step-3.5 rule), and
            // intercepts {defer} Eval requests with an error. If a Request
            // reaches this point, it's an Instantiate/Realise request being
            // spliced into source text — almost certainly a user error.
            OValue::Request {
                kind, fingerprint, ..
            } => {
                let k = Self::kind_tag(kind);
                format!("<request:{} fp={}>", k, &fingerprint[..8])
            }

            // A Thunk has the same splice shape as a NixExpr: its body is
            // valid source text in some language and can be parenthesised
            // inline. The Thunk itself is rarely spliced directly — it
            // usually flows wrapped in a Request[Eval].
            OValue::Thunk { body, .. } => body.clone(),

            // A Group is a control/topology value, not a data value — like a
            // Request, it has no natural splice form. We splice a marker so a
            // Group accidentally embedded in source text is visible rather than
            // silently rendered as a misleading list. Users who want member
            // values spliced should force the group first with `now(...)`.
            OValue::Group {
                mode,
                members,
                fingerprint,
            } => {
                format!(
                    "<group:{} n={} fp={}>",
                    mode.name(),
                    members.len(),
                    &fingerprint[..8]
                )
            }

            // An Expr splices as its source text. Splicing a quoted expression
            // into another backend's source text is rarely useful (the user
            // almost always wants O.eval to run it first), but the conservative
            // fallback is to embed the O source so at least the content is
            // visible rather than silently lost.
            OValue::Expr { src } => src.clone(),

            // A System splices as its profile path. The receiving context
            // gets the symlink path as a string — useful for shell scripts
            // that want to inspect the current generation (`readlink -f $sys`),
            // less useful for source-text-bearing contexts which probably
            // shouldn't be embedding a profile path. Document for now;
            // STEP5 may refine.
            OValue::System { profile_path } => profile_path.clone(),

            OValue::Capability { kind, identity, .. } => {
                format!("<capability:{} {}>", kind.name(), identity)
            }

            OValue::Snapshot { kind, identity, .. } => {
                format!("<snapshot:{} {}>", kind.name(), identity)
            }

            // An Error value splices as a human-readable marker — the raw
            // error text surrounded by brackets. Splicing an error into source
            // code is almost always a bug; the marker makes it visible.
            OValue::Error { msg } => format!("<error: {}>", msg),
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// SECTION 6: Display
//
// Human-readable representation for REPL output and error messages.
// Distinct from splice_repr (which is for code injection) and from the hosted
// wire schema (which is for process communication).
// ═════════════════════════════════════════════════════════════════════════════

impl fmt::Display for OValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OValue::Null => write!(f, "null"),
            OValue::Bool { v } => write!(f, "{}", v),
            OValue::Int { v } => write!(f, "{}", v),
            OValue::Float { v } => write!(f, "{}", v),
            OValue::Number { v } => write!(f, "{}", number_repr(v)),
            OValue::Str { v } => write!(f, "{:?}", v),
            OValue::Text { v } => write!(f, "{:?}", v.utf8),
            OValue::Char { scalar } => write!(f, "{:?}", scalar),
            OValue::List { v } => {
                write!(f, "[")?;
                for (i, item) in v.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", item)?;
                }
                write!(f, "]")
            }
            OValue::Map { v } => {
                write!(f, "{{")?;
                let mut entries = v.iter().collect::<Vec<_>>();
                entries.sort_by_key(|(key, _)| *key);
                for (i, (k, val)) in entries.into_iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{:?}: {}", k, val)?;
                }
                write!(f, "}}")
            }
            OValue::Seq { kind, items } => {
                let (open, close) = match kind {
                    SeqKind::Tuple => ("(", ")"),
                    SeqKind::List | SeqKind::Vector => ("[", "]"),
                };
                write!(f, "{}", open)?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", item)?;
                }
                write!(f, "{}", close)
            }
            OValue::Object { fields } => {
                write!(f, "{{")?;
                for (i, (k, val)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{:?}: {}", k, val)?;
                }
                write!(f, "}}")
            }
            OValue::EntriesMap { entries } => {
                write!(f, "{{")?;
                for (i, (k, val)) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{} => {}", k, val)?;
                }
                write!(f, "}}")
            }
            OValue::Set { items, .. } => {
                write!(f, "{{")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", item)?;
                }
                write!(f, "}}")
            }
            OValue::Symbol { v } => write!(f, "{}", symbol_repr(v)),
            OValue::Keyword { v } => write!(f, "{}", keyword_repr(v)),
            OValue::Scope { bindings } => write!(f, "<scope bindings={}>", bindings.len()),
            OValue::Html { v } => write!(f, "{}", v),
            OValue::StorePath { path } => write!(f, "{}", path),
            OValue::Blob { mime, .. } => write!(f, "<blob:{}>", mime),
            OValue::Bytes { v } => match &v.media_type {
                Some(media_type) => write!(f, "<bytes:{} {}>", media_type, v.bytes.len()),
                None => write!(f, "<bytes:{}>", v.bytes.len()),
            },
            OValue::Graph { root, nodes } => {
                write!(f, "<graph root={} nodes={}>", root, nodes.len())
            }
            OValue::Native { v } => {
                write!(f, "<native {} {} codec={}>", v.lang, v.type_name, v.codec)
            }
            OValue::NixExpr {
                fingerprint, deps, ..
            } => {
                write!(f, "<nix_expr fp={} deps={}>", &fingerprint[..8], deps.len())
            }
            OValue::Derivation {
                drv_path, outputs, ..
            } => {
                write!(
                    f,
                    "<derivation {} outputs=[{}]>",
                    drv_path,
                    outputs.join(",")
                )
            }
            OValue::Request {
                kind, fingerprint, ..
            } => {
                let k = Self::kind_tag(kind);
                write!(f, "<request {} fp={}>", k, &fingerprint[..8])
            }
            OValue::Thunk {
                fingerprint, deps, ..
            } => {
                write!(f, "<thunk fp={} deps={}>", &fingerprint[..8], deps.len())
            }
            OValue::Group {
                mode,
                members,
                fingerprint,
            } => {
                write!(
                    f,
                    "<group {} n={} fp={}>",
                    mode.name(),
                    members.len(),
                    &fingerprint[..8]
                )
            }
            OValue::System { profile_path } => {
                write!(f, "<system {}>", profile_path)
            }
            OValue::Capability {
                kind,
                identity,
                metadata,
            } => {
                write!(
                    f,
                    "<capability {} {} meta={}>",
                    kind.name(),
                    identity,
                    metadata.len()
                )
            }
            OValue::Snapshot {
                kind,
                identity,
                state,
            } => {
                write!(
                    f,
                    "<snapshot {} {} fields={}>",
                    kind.name(),
                    identity,
                    state.len()
                )
            }
            OValue::Expr { src } => {
                // Show a truncated preview of the source — the full text can
                // be arbitrarily long. 40 chars is enough to identify the quote.
                let preview: String = src.chars().take(40).collect();
                if src.len() > 40 {
                    write!(f, "<expr {:?}…>", preview)
                } else {
                    write!(f, "<expr {:?}>", preview)
                }
            }
            OValue::Error { msg } => write!(f, "<error: {}>", msg),
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// SECTION 7: Wire protocol message types
//
// These are the three message types that O's runtime sends to backend
// subprocess shims. The shims respond with OWireResponse.
//
// The protocol is synchronous and binary-framed:
//   → 4-byte big-endian payload length + canonical CBOR command
//   ← 4-byte big-endian payload length + canonical CBOR response
//
// The serde shape below is the schema. `wire.rs` is the real transport codec.
// ═════════════════════════════════════════════════════════════════════════════

/// A command from the O runtime to a backend subprocess shim.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum OWireCommand {
    /// Execute a code string in the backend's current environment.
    /// `bindings` are variables to inject before execution — the resolved
    /// values of any `$var` references that appeared in the expression body.
    Exec {
        code: String,
        bindings: HashMap<String, OValue>,
    },

    /// Clear the backend's environment and release all resources.
    /// Sent when a persistent env [n] is garbage collected, or on shutdown.
    Cleanup,

    /// Optional protocol probe for diagnostics and direct tests.
    /// Backend startup sends real work directly; there is no health-check gate.
    Ping,

    /// The result of evaluating an `eval_request` sent by the shim.
    ///
    /// Sent by the runtime in response to an `OWireResponse::EvalRequest`
    /// from the shim. The `value` field carries the evaluated OValue so the
    /// shim's `O.eval(q)` call can return it to user code and resume
    /// execution of the current block.
    #[serde(rename = "eval_result")]
    EvalResult { value: OValue },
}

/// A response from a backend subprocess shim to the O runtime.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum OWireResponse {
    /// The command succeeded. `value` is the result as an OValue.
    Ok { value: OValue },

    /// The command failed. `message` is the error text from the backend
    /// (stack trace, compilation error, runtime exception — whatever the
    /// backend's language provides).
    Err { message: String },

    /// The shim is mid-execution and needs the runtime to evaluate an O
    /// source fragment on its behalf (for `O.eval(q)` in a Python block).
    ///
    /// Protocol: after receiving this, the runtime evaluates `src` against the
    /// explicit Scope when one is supplied, otherwise against the backend call
    /// site's lexical snapshot, and sends back an `OWireCommand::EvalResult`.
    /// The shim then resumes execution of the current block. The exec-reply
    /// cycle completes when the shim finally sends `Ok` or `Err`.
    #[serde(rename = "eval_request")]
    EvalRequest {
        src: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<OValue>,
    },
}

impl OWireResponse {
    pub fn ok(value: OValue) -> Self {
        OWireResponse::Ok { value }
    }

    pub fn err(message: impl Into<String>) -> Self {
        OWireResponse::Err {
            message: message.into(),
        }
    }

    pub fn into_result(self) -> Result<OValue> {
        match self {
            OWireResponse::Ok { value } => Ok(value),
            OWireResponse::Err { message } => bail!("{}", message),
            OWireResponse::EvalRequest { src, .. } => bail!(
                "unexpected eval_request from shim (src: {:?}) — this shim sent \
                 eval_request outside of an O.eval call or the runtime received it \
                 through a non-eval-aware path",
                &src[..src.len().min(60)]
            ),
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// SECTION 8: Error types
// ═════════════════════════════════════════════════════════════════════════════

#[derive(thiserror::Error, Debug)]
pub enum OValueError {
    #[error("Type mismatch: expected {expected}, got {actual}")]
    TypeMismatch {
        expected: &'static str,
        actual: String,
    },

    #[error("Base64 decode failed for OBlob: {0}")]
    Base64Error(#[from] base64::DecodeError),

    #[error("JSON serialization error: {0}")]
    JsonError(#[from] serde_json::Error),
}

// ═════════════════════════════════════════════════════════════════════════════
// SECTION 9: Tests
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_native() -> ONative {
        ONative {
            lang: "python".to_string(),
            implementation: Some("cpython".to_string()),
            version: Some("3.14".to_string()),
            type_name: "decimal.Decimal".to_string(),
            identity: NativeIdentity {
                stable: Some("decimal:1.25".to_string()),
                live: None,
            },
            codec: "repr".to_string(),
            payload: Some(OBytes {
                bytes: b"Decimal('1.25')".to_vec(),
                media_type: Some("text/x-python-repr".to_string()),
            }),
            boundary: NativeBoundary::Pure,
            safety: NativeCodecSafety::SourceBacked,
            capabilities: vec![],
            metadata: BTreeMap::new(),
            rehydrate: RehydratePolicy::Portable,
        }
    }

    /// Every OValue variant must round-trip through JSON without loss.
    /// This test is the foundational correctness guarantee of the wire protocol.
    #[test]
    fn round_trip_all_variants() {
        let cases: Vec<OValue> = vec![
            OValue::null(),
            OValue::bool_(true),
            OValue::bool_(false),
            OValue::int(42),
            OValue::int(-9_999_999_999_999),
            OValue::int(i64::MAX),
            OValue::int(i64::MIN),
            OValue::float(std::f64::consts::PI),
            OValue::float(-0.0),
            OValue::big_int(BigInt::parse_bytes(b"123456789123456789123456789", 10).unwrap()),
            OValue::rational(22, 7).unwrap(),
            // NOTE: f64::INFINITY excluded — JSON RFC 8259 has no infinity repr.
            // serde_json serializes it as null. Custom serializer needed (tracked).
            OValue::str_("hello, world"),
            OValue::text("hello, text"),
            OValue::bytes(
                b"abc".to_vec(),
                Some("application/octet-stream".to_string()),
            ),
            OValue::char_('x'),
            OValue::str_(""),
            OValue::str_("unicode: こんにちは 🦀"),
            OValue::list(vec![
                OValue::int(1),
                OValue::str_("two"),
                OValue::bool_(false),
                OValue::null(),
            ]),
            OValue::map({
                let mut m = HashMap::new();
                m.insert("x".to_string(), OValue::int(10));
                m.insert("y".to_string(), OValue::float(2.5));
                m.insert("nested".to_string(), OValue::list(vec![OValue::null()]));
                m
            }),
            OValue::seq(SeqKind::Vector, vec![OValue::int(1), OValue::int(2)]),
            OValue::tuple(vec![OValue::str_("a"), OValue::str_("b")]),
            OValue::object(BTreeMap::from([("field".to_string(), OValue::bool_(true))])),
            OValue::entries_map(vec![(
                OValue::tuple(vec![OValue::int(1)]),
                OValue::str_("one"),
            )]),
            OValue::set(SetKind::Unordered, vec![OValue::int(2), OValue::int(1)]),
            OValue::symbol("answer"),
            OValue::namespaced_symbol("math", "pi"),
            OValue::keyword("required"),
            OValue::scope(HashMap::from([("answer".to_string(), OValue::int(42))])),
            OValue::blob(b"\x89PNG\r\n", "image/png"),
            OValue::blob(&[], "application/octet-stream"),
            OValue::graph(
                0,
                vec![
                    GraphNode::Value {
                        value: Box::new(OValue::str_("root")),
                    },
                    GraphNode::Ref { target: 0 },
                ],
            ),
            OValue::native(sample_native()),
            // OExpr round-trips its src string.
            OValue::Expr {
                src: "python^(6 * 7)_python".to_string(),
            },
            OValue::Expr { src: String::new() },
            OValue::capability(
                CapabilityKind::Service,
                "svc:boot",
                HashMap::from([("restart".to_string(), OValue::bool_(true))]),
            ),
            OValue::snapshot(
                SnapshotKind::System,
                "generation-42",
                HashMap::from([("kernel".to_string(), OValue::str_("6.9.0"))]),
            ),
            // Group round-trips mode + members + fingerprint.
            OValue::group(GroupMode::Batch, vec![OValue::int(1), OValue::int(2)]),
            OValue::group(GroupMode::Race, vec![OValue::str_("a")]),
        ];

        for original in &cases {
            let json = serde_json::to_string(original)
                .unwrap_or_else(|e| panic!("Serialize failed for {:?}: {}", original, e));
            let decoded: OValue = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("Deserialize failed for {}: {}", json, e));
            assert_eq!(
                *original, decoded,
                "Round-trip failed: {:?} → {} → {:?}",
                original, json, decoded
            );
        }
    }

    /// OWireCommand and OWireResponse must also round-trip cleanly,
    /// since they are what actually travels over the subprocess pipe.
    #[test]
    fn round_trip_wire_messages() {
        use crate::wire;

        let mut bindings = HashMap::new();
        bindings.insert("a".to_string(), OValue::int(10));
        bindings.insert("b".to_string(), OValue::str_("hello"));

        let cmd = OWireCommand::Exec {
            code: "print(a + 1)".to_string(),
            bindings,
        };
        let encoded = wire::encode_message(&cmd).unwrap();
        assert_ne!(encoded.first().copied(), Some(b'{'));
        let _decoded: OWireCommand = wire::decode_message(&encoded).unwrap();

        let resp = OWireResponse::ok(OValue::str_("result"));
        let encoded = wire::encode_message(&resp).unwrap();
        assert_ne!(encoded.first().copied(), Some(b'{'));
        let decoded: OWireResponse = wire::decode_message(&encoded).unwrap();
        assert!(matches!(decoded, OWireResponse::Ok { .. }));

        // EvalRequest/EvalResult round-trip
        let eval_req = OWireResponse::EvalRequest {
            src: "python^(42)_python".to_string(),
            scope: Some(OValue::scope(HashMap::from([(
                "answer".to_string(),
                OValue::int(42),
            )]))),
        };
        let encoded = wire::encode_message(&eval_req).unwrap();
        let decoded: OWireResponse = wire::decode_message(&encoded).unwrap();
        assert!(matches!(decoded, OWireResponse::EvalRequest { .. }));

        let eval_result = OWireCommand::EvalResult {
            value: OValue::int(42),
        };
        let encoded = wire::encode_message(&eval_result).unwrap();
        let decoded: OWireCommand = wire::decode_message(&encoded).unwrap();
        assert!(matches!(decoded, OWireCommand::EvalResult { .. }));
    }

    /// The type_name method must return the exact string used as the
    /// wire protocol `t` tag — they must stay in sync.
    #[test]
    fn type_names_match_wire_tags() {
        let cases = vec![
            (OValue::null(), "null"),
            (OValue::bool_(true), "bool"),
            (OValue::int(0), "int"),
            (OValue::float(0.0), "float"),
            (OValue::big_int(0), "number"),
            (OValue::str_(""), "str"),
            (OValue::text(""), "text"),
            (OValue::bytes(Vec::<u8>::new(), None), "bytes"),
            (OValue::char_('a'), "char"),
            (OValue::list(vec![]), "list"),
            (OValue::map(HashMap::new()), "map"),
            (OValue::seq(SeqKind::List, vec![]), "seq"),
            (OValue::object(BTreeMap::new()), "object"),
            (OValue::entries_map(vec![]), "entries_map"),
            (OValue::set(SetKind::Ordered, vec![]), "set"),
            (OValue::symbol("s"), "symbol"),
            (OValue::keyword("k"), "keyword"),
            (OValue::scope(HashMap::new()), "scope"),
            (OValue::blob(&[], ""), "blob"),
            (OValue::graph(0, vec![]), "graph"),
            (OValue::native(sample_native()), "native"),
            (
                OValue::Expr {
                    src: "x".to_string(),
                },
                "expr",
            ),
            (
                OValue::capability(CapabilityKind::File, "/etc/hosts", HashMap::new()),
                "capability",
            ),
            (
                OValue::snapshot(SnapshotKind::Service, "svc:sshd", HashMap::new()),
                "snapshot",
            ),
            (
                OValue::group(GroupMode::Batch, vec![OValue::int(1)]),
                "group",
            ),
        ];
        for (val, expected_tag) in cases {
            assert_eq!(val.type_name(), expected_tag);
            let json: serde_json::Value =
                serde_json::from_str(&serde_json::to_string(&val).unwrap()).unwrap();
            assert_eq!(
                json["t"].as_str().unwrap(),
                expected_tag,
                "Wire tag mismatch for {}",
                expected_tag
            );
        }
    }

    #[test]
    fn blob_bytes_round_trip() {
        let original_bytes = b"arbitrary binary \x00\x01\x02\xFF data";
        let blob = OValue::blob(original_bytes, "application/octet-stream");
        let recovered = blob.blob_bytes().expect("blob_bytes returned None");
        assert_eq!(original_bytes.as_ref(), recovered.as_slice());
    }

    #[test]
    fn splice_repr_produces_expected_strings() {
        assert_eq!(OValue::null().splice_repr(), "null");
        assert_eq!(OValue::bool_(true).splice_repr(), "true");
        assert_eq!(OValue::int(42).splice_repr(), "42");
        assert_eq!(OValue::float(3.0).splice_repr(), "3.0");
        assert_eq!(OValue::str_("hi").splice_repr(), "hi");
    }

    #[test]
    fn content_identity_uses_canonical_bytes_not_splice_repr() {
        let int_one = OValue::int(1);
        let str_one = OValue::str_("1");

        assert_eq!(int_one.splice_repr(), str_one.splice_repr());
        assert_ne!(int_one.content_identity(), str_one.content_identity());

        let nan_a = OValue::float(f64::from_bits(0x7ff8_0000_0000_0001));
        let nan_b = OValue::float(f64::from_bits(0x7ff8_0000_0000_0002));
        assert_ne!(nan_a.content_identity(), nan_b.content_identity());
    }

    #[test]
    fn runtime_boundary_classifies_live_world_values() {
        let pure = OValue::snapshot(SnapshotKind::System, "gen-1", HashMap::new());
        let referential = OValue::system("/nix/var/nix/profiles/system");
        let effectful = OValue::capability(CapabilityKind::Device, "pci:00:1f.2", HashMap::new());
        let scope = OValue::scope(HashMap::from([("cap".to_string(), effectful.clone())]));

        assert_eq!(pure.runtime_boundary(), RuntimeBoundary::Pure);
        assert_eq!(referential.runtime_boundary(), RuntimeBoundary::Referential);
        assert_eq!(effectful.runtime_boundary(), RuntimeBoundary::Effectful);
        assert_eq!(scope.runtime_boundary(), RuntimeBoundary::Effectful);
        assert!(!scope.is_cache_safe());
        assert!(!scope.is_replay_safe());
        assert!(!scope.is_boot_persistable());
    }

    #[test]
    fn safety_predicates_recurse_into_child_values() {
        let capability = OValue::capability(CapabilityKind::File, "/etc/passwd", HashMap::new());
        let list = OValue::list(vec![capability.clone()]);
        let object = OValue::object(BTreeMap::from([("cap".to_string(), capability.clone())]));
        let entries_map = OValue::entries_map(vec![(OValue::str_("cap"), capability.clone())]);
        let request = OValue::request(
            RequestKind::Eval {
                lang: "python".to_string(),
                env_id: 0,
                cacheable: true,
                authority: None,
                permissions: vec![],
            },
            list.clone(),
        );

        for value in [list, object, entries_map, request] {
            assert!(!value.is_cache_safe());
            assert!(!value.is_replay_safe());
            assert!(!value.is_boot_persistable());
        }
    }

    #[test]
    fn native_capsule_safety_requires_inert_payload() {
        let pure = OValue::native(sample_native());
        assert!(pure.is_cache_safe());
        assert!(pure.is_replay_safe());
        assert!(pure.is_boot_persistable());

        let mut live = sample_native();
        live.identity.live = Some("py-object:0x1".to_string());
        let live = OValue::native(live);

        assert!(!live.is_cache_safe());
        assert!(!live.is_replay_safe());
        assert!(!live.is_boot_persistable());
    }

    #[test]
    fn persistence_and_cache_flags_match_runtime_contract() {
        let lazy_req = OValue::request(
            RequestKind::Eval {
                lang: "python".to_string(),
                env_id: 0,
                cacheable: true,
                authority: None,
                permissions: vec![],
            },
            OValue::thunk("41 + 1", vec![]),
        );
        let activate_req = OValue::request(
            RequestKind::Activate {
                profile: "/nix/var/nix/profiles/system".to_string(),
                dry_run: true,
                authority: None,
            },
            OValue::store_path("/nix/store/demo-system"),
        );
        let authority_req = OValue::request(
            RequestKind::Eval {
                lang: "python".to_string(),
                env_id: 0,
                cacheable: true,
                authority: Some("o-backend-live:test".to_string()),
                permissions: vec![BackendAuthority::Process],
            },
            OValue::thunk("41 + 1", vec![]),
        );
        let system = OValue::system("/nix/var/nix/profiles/system");
        let snapshot = OValue::snapshot(SnapshotKind::System, "gen-42", HashMap::new());

        assert!(lazy_req.is_cache_safe());
        assert!(!activate_req.is_cache_safe());
        assert!(!activate_req.is_replay_safe());
        assert!(!authority_req.is_cache_safe());
        assert!(!authority_req.is_replay_safe());
        assert!(!authority_req.is_boot_persistable());
        assert!(!system.is_cache_safe());
        assert!(snapshot.is_boot_persistable());
        assert!(!system.is_boot_persistable());
        assert!(!system.is_replay_safe());
        assert!(snapshot.is_replay_safe());
    }

    /// ONixExpr constructor must compute a stable sha256(body) fingerprint,
    /// store deps by reference, and round-trip through JSON without loss.
    #[test]
    fn nix_expr_fingerprint_is_sha256_of_body() {
        let body = "pkgs.hello";
        let val = OValue::nix_expr(body, vec![]);

        if let OValue::NixExpr { fingerprint, .. } = &val {
            // sha256("pkgs.hello") = 6b0fc1cf4a0e73a498b0bd6b0d0e6ab91e01bc59…
            // Just verify it is a 64-hex-character string (256 bits).
            assert_eq!(
                fingerprint.len(),
                64,
                "fingerprint should be 64 hex chars (sha256), got {:?}",
                fingerprint
            );
            assert!(
                fingerprint.chars().all(|c| c.is_ascii_hexdigit()),
                "fingerprint must be hex, got {:?}",
                fingerprint
            );
        } else {
            panic!("expected OValue::NixExpr, got {:?}", val);
        }
    }

    #[test]
    fn nix_expr_same_body_produces_same_fingerprint() {
        let a = OValue::nix_expr("pkgs.hello", vec![]);
        let b = OValue::nix_expr("pkgs.hello", vec![]);
        if let (
            OValue::NixExpr {
                fingerprint: fa, ..
            },
            OValue::NixExpr {
                fingerprint: fb, ..
            },
        ) = (&a, &b)
        {
            assert_eq!(
                fa, fb,
                "identical bodies must produce identical fingerprints"
            );
        }
    }

    #[test]
    fn nix_expr_different_body_produces_different_fingerprint() {
        let a = OValue::nix_expr("pkgs.hello", vec![]);
        let b = OValue::nix_expr("pkgs.world", vec![]);
        if let (
            OValue::NixExpr {
                fingerprint: fa, ..
            },
            OValue::NixExpr {
                fingerprint: fb, ..
            },
        ) = (&a, &b)
        {
            assert_ne!(
                fa, fb,
                "different bodies must produce different fingerprints"
            );
        }
    }

    #[test]
    fn nix_expr_deps_are_stored_by_reference() {
        let dep = OValue::str_("a_dep");
        let val = OValue::nix_expr("some expr", vec![dep.clone()]);
        if let OValue::NixExpr { deps, .. } = &val {
            assert_eq!(deps.len(), 1);
            assert_eq!(deps[0], dep);
        } else {
            panic!("expected OValue::NixExpr");
        }
    }

    #[test]
    fn nix_expr_round_trips_through_json() {
        let dep = OValue::int(42);
        let original = OValue::nix_expr("(builtins.add 1 2)", vec![dep]);
        let json = serde_json::to_string(&original).unwrap();
        let decoded: OValue = serde_json::from_str(&json).unwrap();
        assert_eq!(original, decoded, "ONixExpr must round-trip through JSON");
    }

    #[test]
    fn nix_expr_type_name_is_nix_expr() {
        let val = OValue::nix_expr("x", vec![]);
        assert_eq!(val.type_name(), "nix_expr");
        assert!(val.is_nix_expr());
    }

    #[test]
    fn nix_expr_splice_repr_is_body() {
        let body = "pkgs.curl";
        let val = OValue::nix_expr(body, vec![]);
        assert_eq!(val.splice_repr(), body);
    }

    // ── STEP-2 FINGERPRINT COMPOSITION ───────────────────────────────────────

    /// The upgraded step-2 fingerprint must change when deps change, even if
    /// the body text is identical. This is what makes the cache key honest
    /// when the same Nix source text uses different upstream derivations.
    #[test]
    fn nix_expr_fingerprint_depends_on_deps() {
        let dep_a = OValue::store_path("/nix/store/aaa-foo");
        let dep_b = OValue::store_path("/nix/store/bbb-bar");
        let with_a = OValue::nix_expr("pkgs.x", vec![dep_a]);
        let with_b = OValue::nix_expr("pkgs.x", vec![dep_b]);
        if let (
            OValue::NixExpr {
                fingerprint: fa, ..
            },
            OValue::NixExpr {
                fingerprint: fb, ..
            },
        ) = (&with_a, &with_b)
        {
            assert_ne!(
                fa, fb,
                "same body + different deps must produce different fingerprints"
            );
        }
    }

    /// Dep order must not affect the fingerprint (we sort identities before hashing).
    #[test]
    fn nix_expr_fingerprint_dep_order_invariant() {
        let a = OValue::store_path("/nix/store/a");
        let b = OValue::store_path("/nix/store/b");
        let ab = OValue::nix_expr("x", vec![a.clone(), b.clone()]);
        let ba = OValue::nix_expr("x", vec![b, a]);
        if let (
            OValue::NixExpr {
                fingerprint: fab, ..
            },
            OValue::NixExpr {
                fingerprint: fba, ..
            },
        ) = (&ab, &ba)
        {
            assert_eq!(
                fab, fba,
                "dep order must not affect fingerprint (identities are sorted)"
            );
        }
    }

    // ── DERIVATION ───────────────────────────────────────────────────────────

    #[test]
    fn derivation_constructor_stores_fields() {
        let d = OValue::derivation(
            "/nix/store/abc-foo.drv",
            vec!["out".into(), "dev".into()],
            vec![],
        );
        if let OValue::Derivation {
            drv_path,
            outputs,
            deps,
        } = &d
        {
            assert_eq!(drv_path, "/nix/store/abc-foo.drv");
            assert_eq!(outputs, &vec!["out".to_string(), "dev".to_string()]);
            assert!(deps.is_empty());
        } else {
            panic!("expected Derivation");
        }
    }

    #[test]
    fn derivation_content_identity_is_hash_of_drv_path() {
        let d = OValue::derivation("/nix/store/abc.drv", vec!["out".into()], vec![]);
        let id = d.content_identity();
        assert_eq!(id.len(), 64);
        // Same drv_path → same identity
        let d2 = OValue::derivation("/nix/store/abc.drv", vec![], vec![]);
        assert_eq!(
            id,
            d2.content_identity(),
            "content_identity depends only on drv_path, not outputs/deps"
        );
    }

    #[test]
    fn derivation_type_name_and_round_trip() {
        let d = OValue::derivation("/nix/store/x.drv", vec!["out".into()], vec![]);
        assert_eq!(d.type_name(), "derivation");
        assert!(d.is_derivation());
        let json = serde_json::to_string(&d).unwrap();
        let decoded: OValue = serde_json::from_str(&json).unwrap();
        assert_eq!(d, decoded);
    }

    // ── REQUEST ──────────────────────────────────────────────────────────────

    #[test]
    fn request_construction_carries_kind_and_source() {
        let expr = OValue::nix_expr("pkgs.hello", vec![]);
        let req = OValue::request(RequestKind::Instantiate, expr.clone());
        if let OValue::Request {
            kind,
            source,
            fingerprint,
        } = &req
        {
            assert_eq!(*kind, RequestKind::Instantiate);
            assert_eq!(**source, expr);
            assert_eq!(fingerprint.len(), 64);
        } else {
            panic!("expected Request");
        }
    }

    #[test]
    fn request_fingerprint_composes_from_source_identity() {
        let e1 = OValue::nix_expr("pkgs.hello", vec![]);
        let e2 = OValue::nix_expr("pkgs.hello", vec![]);
        let r1 = OValue::request(RequestKind::Instantiate, e1);
        let r2 = OValue::request(RequestKind::Instantiate, e2);
        if let (
            OValue::Request {
                fingerprint: f1, ..
            },
            OValue::Request {
                fingerprint: f2, ..
            },
        ) = (&r1, &r2)
        {
            assert_eq!(
                f1, f2,
                "two requests with identical-content sources must share a fingerprint"
            );
        }
    }

    #[test]
    fn request_fingerprint_differs_by_kind() {
        // Two requests over the same source but different kinds must differ.
        // (Realise-over-NixExpr is type-incorrect at execution but the
        //  fingerprint computation doesn't care about that — it's purely
        //  content-based.)
        let src = OValue::nix_expr("pkgs.hello", vec![]);
        let r_inst = OValue::request(RequestKind::Instantiate, src.clone());
        let r_real = OValue::request(RequestKind::Realise, src);
        if let (
            OValue::Request {
                fingerprint: fi, ..
            },
            OValue::Request {
                fingerprint: fr, ..
            },
        ) = (&r_inst, &r_real)
        {
            assert_ne!(
                fi, fr,
                "requests must be distinguished by kind in their fingerprint"
            );
        }
    }

    /// Chained requests (e.g. realise(instantiate(expr))) compose cleanly:
    /// the outer fingerprint is determined by the inner request's fingerprint,
    /// not by walking into the inner source.
    #[test]
    fn request_chain_fingerprint_is_stable() {
        let expr = OValue::nix_expr("pkgs.hello", vec![]);
        let inst = OValue::request(RequestKind::Instantiate, expr);
        let real = OValue::request(RequestKind::Realise, inst.clone());

        if let OValue::Request {
            fingerprint: outer, ..
        } = &real
        {
            // Recomputing with the same inner produces the same outer fp
            let real2 = OValue::request(RequestKind::Realise, inst);
            if let OValue::Request {
                fingerprint: outer2,
                ..
            } = &real2
            {
                assert_eq!(
                    outer, outer2,
                    "chained request fingerprint must be stable across reconstructions"
                );
            }
        }
    }

    #[test]
    fn request_type_name_and_round_trip() {
        let expr = OValue::nix_expr("pkgs.hello", vec![]);
        let req = OValue::request(RequestKind::Instantiate, expr);
        assert_eq!(req.type_name(), "request");
        assert!(req.is_request());
        let json = serde_json::to_string(&req).unwrap();
        let decoded: OValue = serde_json::from_str(&json).unwrap();
        assert_eq!(req, decoded);
    }

    // ── STEP-4: SYSTEM ───────────────────────────────────────────────────────

    #[test]
    fn system_constructor_and_predicates() {
        let s = OValue::system("/nix/var/nix/profiles/system");
        assert!(s.is_system());
        assert_eq!(s.type_name(), "system");
        if let OValue::System { profile_path } = &s {
            assert_eq!(profile_path, "/nix/var/nix/profiles/system");
        } else {
            panic!("expected System");
        }
    }

    #[test]
    fn system_content_identity_is_referential() {
        // Two Systems with the same profile path have the same identity,
        // regardless of any out-of-band state. This is the structural
        // concession that lets a System participate in the value model
        // without pretending to be a snapshot.
        let a = OValue::system("/nix/var/nix/profiles/system");
        let b = OValue::system("/nix/var/nix/profiles/system");
        assert_eq!(a.content_identity(), b.content_identity());

        let c = OValue::system("/home/lee/.nix-profile");
        assert_ne!(a.content_identity(), c.content_identity());
    }

    #[test]
    fn system_round_trips_through_json() {
        let s = OValue::system("/nix/var/nix/profiles/system");
        let json = serde_json::to_string(&s).unwrap();
        let decoded: OValue = serde_json::from_str(&json).unwrap();
        assert_eq!(s, decoded);
    }

    // ── STEP-4: ACTIVATE REQUEST ─────────────────────────────────────────────

    #[test]
    fn activate_request_construction() {
        let path = OValue::store_path("/nix/store/abc-system");
        let req = OValue::request(
            RequestKind::Activate {
                profile: "/nix/var/nix/profiles/system".into(),
                dry_run: true,
                authority: None,
            },
            path,
        );
        if let OValue::Request {
            kind,
            source,
            fingerprint,
        } = &req
        {
            if let RequestKind::Activate {
                profile, dry_run, ..
            } = kind
            {
                assert_eq!(profile, "/nix/var/nix/profiles/system");
                assert!(*dry_run);
            } else {
                panic!("expected Activate kind");
            }
            assert!(matches!(source.as_ref(), OValue::StorePath { .. }));
            assert_eq!(fingerprint.len(), 64);
        } else {
            panic!("expected Request");
        }
    }

    /// Activate fingerprints distinguish dry-run vs. real activation.
    /// Caching a dry-run result must not satisfy a real-activation request.
    #[test]
    fn activate_fingerprint_distinguishes_dry_run() {
        let path = OValue::store_path("/nix/store/abc");
        let dry = OValue::request(
            RequestKind::Activate {
                profile: "/p".into(),
                dry_run: true,
                authority: None,
            },
            path.clone(),
        );
        let wet = OValue::request(
            RequestKind::Activate {
                profile: "/p".into(),
                dry_run: false,
                authority: Some("o-activate-live:test".into()),
            },
            path,
        );
        if let (
            OValue::Request {
                fingerprint: f_dry, ..
            },
            OValue::Request {
                fingerprint: f_wet, ..
            },
        ) = (&dry, &wet)
        {
            assert_ne!(f_dry, f_wet);
        }
    }

    // ── STEP-4 GROUP COORDINATION PRIMITIVES ─────────────────────────────────

    #[test]
    fn group_constructor_preserves_mode_and_members() {
        let members = vec![OValue::int(1), OValue::int(2), OValue::int(3)];
        let g = OValue::group(GroupMode::Batch, members.clone());
        if let OValue::Group {
            mode,
            members: m,
            fingerprint,
        } = &g
        {
            assert_eq!(*mode, GroupMode::Batch);
            assert_eq!(m, &members);
            assert_eq!(
                fingerprint.len(),
                64,
                "group fingerprint must be sha256 hex"
            );
            assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
        } else {
            panic!("expected OValue::Group, got {:?}", g);
        }
        assert_eq!(g.type_name(), "group");
        assert!(g.is_group());
    }

    #[test]
    fn group_mode_changes_fingerprint() {
        let members = vec![OValue::int(1), OValue::int(2)];
        let batch = OValue::group(GroupMode::Batch, members.clone());
        let all = OValue::group(GroupMode::All, members.clone());
        let any = OValue::group(GroupMode::Any, members.clone());
        let race = OValue::group(GroupMode::Race, members);
        let fps: Vec<String> = [batch, all, any, race]
            .iter()
            .map(|g| g.content_identity())
            .collect();
        // All four modes over the same members must have distinct identities.
        for i in 0..fps.len() {
            for j in (i + 1)..fps.len() {
                assert_ne!(fps[i], fps[j], "mode {i} vs {j} must differ");
            }
        }
    }

    #[test]
    fn group_member_order_is_significant() {
        let a = OValue::group(GroupMode::Batch, vec![OValue::int(1), OValue::int(2)]);
        let b = OValue::group(GroupMode::Batch, vec![OValue::int(2), OValue::int(1)]);
        assert_ne!(
            a.content_identity(),
            b.content_identity(),
            "member order must change the group fingerprint (order is semantic)"
        );
    }

    #[test]
    fn group_same_inputs_produce_same_fingerprint() {
        let a = OValue::group(GroupMode::All, vec![OValue::str_("x"), OValue::str_("y")]);
        let b = OValue::group(GroupMode::All, vec![OValue::str_("x"), OValue::str_("y")]);
        assert_eq!(a.content_identity(), b.content_identity());
    }

    #[test]
    fn group_round_trips_through_json() {
        let original = OValue::group(
            GroupMode::Race,
            vec![OValue::int(1), OValue::str_("two"), OValue::bool_(true)],
        );
        let json = serde_json::to_string(&original).unwrap();
        let decoded: OValue = serde_json::from_str(&json).unwrap();
        assert_eq!(original, decoded, "Group must round-trip through JSON");
    }

    #[test]
    fn group_mode_serializes_snake_case() {
        let json =
            serde_json::to_string(&OValue::group(GroupMode::Batch, vec![OValue::int(1)])).unwrap();
        assert!(json.contains("\"mode\":\"batch\""), "got {json}");
        assert!(json.contains("\"t\":\"group\""), "got {json}");
    }

    #[test]
    fn group_mode_name_and_collects_all() {
        assert_eq!(GroupMode::Batch.name(), "batch");
        assert_eq!(GroupMode::All.name(), "all");
        assert_eq!(GroupMode::Any.name(), "any");
        assert_eq!(GroupMode::Race.name(), "race");
        assert!(GroupMode::Batch.collects_all());
        assert!(GroupMode::All.collects_all());
        assert!(!GroupMode::Any.collects_all());
        assert!(!GroupMode::Race.collects_all());
    }

    #[test]
    fn group_splice_repr_is_a_marker() {
        let g = OValue::group(GroupMode::Batch, vec![OValue::int(1)]);
        let s = g.splice_repr();
        assert!(
            s.starts_with("<group:batch"),
            "group splice marker, got {s}"
        );
    }
}
