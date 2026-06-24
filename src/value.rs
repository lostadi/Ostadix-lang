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
// The answer is a sum type with eight variants. That's it. That's the entire
// O value universe. All inter-language richness lives in how backends
// serialize their native values into these eight shapes, and deserialize them
// back out. The wire protocol (JSON over stdin/stdout) is also defined here,
// because the encoding and the type are inseparable.
//
// Design note on OInt: we use i64 for the MVP. This is a known limitation —
// arbitrary precision integers exist in Python, Haskell, and Lisp, and they
// cannot round-trip through i64 without loss. The fix (num-bigint) is
// straightforward and will be added before the first public release.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::fmt;

use anyhow::{bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use hex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ═════════════════════════════════════════════════════════════════════════════
// SECTION 1: The OValue Sum Type
// ═════════════════════════════════════════════════════════════════════════════

/// The complete universe of values in the O language runtime.
///
/// Every value that passes between language backends — from Python to HTML,
/// from Racket to LaTeX, from Haskell to Rust — is one of these eight
/// variants. The type is the wire protocol: `serde` derives the JSON encoding
/// automatically from the struct shape.
///
/// Encoding schema (each variant's JSON representation):
///   ONull               → `{"t":"null"}`
///   OBool(true)         → `{"t":"bool","v":true}`
///   OInt(42)            → `{"t":"int","v":42}`
///   OFloat(3.14)        → `{"t":"float","v":3.14}`
///   OStr("hi")          → `{"t":"str","v":"hi"}`
///   OList([...])        → `{"t":"list","v":[...]}`
///   OMap({...})         → `{"t":"map","v":{...}}`
///   OBlob{..}           → `{"t":"blob","v":"<base64>","mime":"image/png"}`
///
/// The `t` tag is the type discriminant. The `v` field carries the payload.
/// OBlob has an additional `mime` field because the blob's type information
/// is semantic — an HTML backend needs to know whether a blob is a PNG
/// (render as <img>), an HTML fragment (embed directly), or a PDF (link out).
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

    /// A UTF-8 string. The most common inter-language value type.
    /// Raw text from backends, spliced $var values, document content —
    /// all arrive as OStr unless the backend explicitly returns something richer.
    #[serde(rename = "str")]
    Str { v: String },
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
    /// `dry_run` defaults to true at construction. A real activation carries
    /// the opaque identity of a live, profile-scoped SystemActivation
    /// capability. The evaluator resolves that identity through its private
    /// authority table before the operation can reach the perform boundary.
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
    pub fn str_(s: impl Into<String>) -> Self {
        OValue::Str { v: s.into() }
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
    /// - Other values use SHA-256 of their stable splice representation.
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
                let s = other.splice_repr();
                hex::encode(Sha256::digest(s.as_bytes()))
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
        matches!(self, OValue::Int { .. })
    }
    pub fn is_float(&self) -> bool {
        matches!(self, OValue::Float { .. })
    }
    pub fn is_str(&self) -> bool {
        matches!(self, OValue::Str { .. })
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
    pub fn is_scope(&self) -> bool {
        matches!(self, OValue::Scope { .. })
    }
    pub fn is_blob(&self) -> bool {
        matches!(self, OValue::Blob { .. })
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
        self.is_int() || self.is_float()
    }

    /// The type name as it appears in the wire protocol `t` field.
    pub fn type_name(&self) -> &'static str {
        match self {
            OValue::Null => "null",
            OValue::Bool { .. } => "bool",
            OValue::Int { .. } => "int",
            OValue::Float { .. } => "float",
            OValue::Str { .. } => "str",
            OValue::Html { .. } => "html",
            OValue::StorePath { .. } => "store_path",
            OValue::List { .. } => "list",
            OValue::Map { .. } => "map",
            OValue::Scope { .. } => "scope",
            OValue::Blob { .. } => "blob",
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
            _ => RuntimeBoundary::Pure,
        }
    }

    /// Whether this value can be encoded and shipped across process boundaries.
    ///
    /// Today every `OValue` is serializable via the JSON wire format.
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
                ..
            } => *cacheable && authority.is_none() && permissions.is_empty(),
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
                ..
            } => authority.is_none() && permissions.is_empty(),
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
                ..
            } => authority.is_none() && permissions.is_empty(),
            _ => true,
        }
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
            other => bail!("Expected int, got {}", other.type_name()),
        }
    }

    pub fn as_float(&self) -> Result<f64> {
        match self {
            OValue::Float { v } => Ok(*v),
            // Implicit int → float widening, because this is always safe
            OValue::Int { v } => Ok(*v as f64),
            other => bail!("Expected float, got {}", other.type_name()),
        }
    }

    pub fn as_str(&self) -> Result<&str> {
        match self {
            OValue::Str { v } => Ok(v.as_str()),
            other => bail!("Expected str, got {}", other.type_name()),
        }
    }

    pub fn as_list(&self) -> Result<&Vec<OValue>> {
        match self {
            OValue::List { v } => Ok(v),
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
            OValue::Str { v } => v.clone(),
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
            OValue::Scope { bindings } => format!("<scope bindings={}>", bindings.len()),
            OValue::Blob { v, mime } => {
                format!("data:{};base64,{}", mime, v)
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
// Distinct from splice_repr (which is for code injection) and from the
// JSON wire format (which is for process communication).
// ═════════════════════════════════════════════════════════════════════════════

impl fmt::Display for OValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OValue::Null => write!(f, "null"),
            OValue::Bool { v } => write!(f, "{}", v),
            OValue::Int { v } => write!(f, "{}", v),
            OValue::Float { v } => write!(f, "{}", v),
            OValue::Str { v } => write!(f, "{:?}", v),
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
            OValue::Scope { bindings } => write!(f, "<scope bindings={}>", bindings.len()),
            OValue::Html { v } => write!(f, "{}", v),
            OValue::StorePath { path } => write!(f, "{}", path),
            OValue::Blob { mime, .. } => write!(f, "<blob:{}>", mime),
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
// The protocol is synchronous and line-delimited:
//   → one JSON object per line to the shim's stdin
//   ← one JSON object per line from the shim's stdout
//
// This is intentionally simple. The shim's job is to be thin.
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

    /// Verify the backend process is alive and responsive.
    /// Used by the process manager before sending real work.
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
            // NOTE: f64::INFINITY excluded — JSON RFC 8259 has no infinity repr.
            // serde_json serializes it as null. Custom serializer needed (tracked).
            OValue::str_("hello, world"),
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
            OValue::scope(HashMap::from([("answer".to_string(), OValue::int(42))])),
            OValue::blob(b"\x89PNG\r\n", "image/png"),
            OValue::blob(&[], "application/octet-stream"),
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
        let mut bindings = HashMap::new();
        bindings.insert("a".to_string(), OValue::int(10));
        bindings.insert("b".to_string(), OValue::str_("hello"));

        let cmd = OWireCommand::Exec {
            code: "print(a + 1)".to_string(),
            bindings,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let _decoded: OWireCommand = serde_json::from_str(&json).unwrap();
        // Verify the cmd tag is present and correct
        assert!(json.contains(r#""cmd":"exec""#));

        let resp = OWireResponse::ok(OValue::str_("result"));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""status":"ok""#));
        let decoded: OWireResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, OWireResponse::Ok { .. }));

        // EvalRequest/EvalResult round-trip
        let eval_req = OWireResponse::EvalRequest {
            src: "python^(42)_python".to_string(),
            scope: Some(OValue::scope(HashMap::from([(
                "answer".to_string(),
                OValue::int(42),
            )]))),
        };
        let json = serde_json::to_string(&eval_req).unwrap();
        assert!(json.contains(r#""status":"eval_request""#));
        assert!(json.contains(r#""src":"python^(42)_python""#));
        assert!(json.contains(r#""scope":{"t":"scope""#));
        let decoded: OWireResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, OWireResponse::EvalRequest { .. }));

        let eval_result = OWireCommand::EvalResult {
            value: OValue::int(42),
        };
        let json = serde_json::to_string(&eval_result).unwrap();
        assert!(json.contains(r#""cmd":"eval_result""#));
        let decoded: OWireCommand = serde_json::from_str(&json).unwrap();
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
            (OValue::str_(""), "str"),
            (OValue::list(vec![]), "list"),
            (OValue::map(HashMap::new()), "map"),
            (OValue::scope(HashMap::new()), "scope"),
            (OValue::blob(&[], ""), "blob"),
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
