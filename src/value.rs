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
use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;


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
    Html  { v: String },
    #[serde(rename = "store_path")]
    StorePath { path: String },

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
    Blob { v: String, mime: String },   // v = base64-encoded bytes on wire

    /// A lazy Nix expression that has not yet been passed to `nix eval`.
    ///
    /// This is the "deferred drv rung" value produced by `nix_expr^(...)_nix_expr`
    /// blocks. It holds:
    ///   - `body`:        the fully-spliced Nix source text (ready to hand to nix eval)
    ///   - `deps`:        the child OValues whose rendered forms were spliced into body,
    ///                    carried by reference (step 1 decision — simpler, lets the
    ///                    renderer re-traverse the tree if needed)
    ///   - `fingerprint`: sha256(body) hex string — cheap cache key (step 1 decision;
    ///                    upgraded to sha256(body + sorted dep fingerprints) in step 2)
    ///
    /// `nix^(...)_nix` is unchanged — it is the "evaluate immediately to a JSON value"
    /// shortcut that bypasses this rung entirely (step 1 decision, option a).
    #[serde(rename = "nix_expr")]
    NixExpr {
        body:        String,
        deps:        Vec<OValue>,
        fingerprint: String,
    },
}


// ═════════════════════════════════════════════════════════════════════════════
// SECTION 2: Constructors
//
// Ergonomic constructors so call sites don't have to write
// OValue::Str { v: "hello".to_string() } everywhere.
// ═════════════════════════════════════════════════════════════════════════════

impl OValue {
    pub fn null()                   -> Self { OValue::Null }
    pub fn bool_(b: bool)           -> Self { OValue::Bool  { v: b } }
    pub fn int(n: i64)              -> Self { OValue::Int   { v: n } }
    pub fn float(f: f64)            -> Self { OValue::Float { v: f } }
    pub fn str_(s: impl Into<String>) -> Self { OValue::Str { v: s.into() } }
    pub fn html(s: impl Into<String>) -> Self { OValue::Html { v: s.into() } }
    pub fn store_path(path: impl Into<String>) -> Self { OValue::StorePath { path: path.into() } }
    pub fn list(items: Vec<OValue>) -> Self { OValue::List  { v: items } }
    pub fn map(entries: HashMap<String, OValue>) -> Self { OValue::Map { v: entries } }

    /// Construct a lazy Nix expression value.
    ///
    /// `body` is the fully-spliced Nix source text.
    /// `deps` are the child OValues (by reference) whose rendered forms were
    /// spliced into `body`.
    ///
    /// The fingerprint is computed as `sha256(body)` — the cheap step-1 scheme.
    /// It will be upgraded to `sha256(body + sorted(dep.fingerprint for dep in deps))`
    /// in step 2 when `ODerivation` and `Request[Climb]` arrive.
    pub fn nix_expr(body: impl Into<String>, deps: Vec<OValue>) -> Self {
        let body = body.into();
        let fingerprint = hex::encode(Sha256::digest(body.as_bytes()));
        OValue::NixExpr { body, deps, fingerprint }
    }

    /// Construct an OBlob from raw bytes and a MIME type.
    /// The bytes are base64-encoded here; the wire representation stores
    /// the base64 string, not the raw bytes.
    pub fn blob(data: &[u8], mime: impl Into<String>) -> Self {
        OValue::Blob {
            v:    B64.encode(data),
            mime: mime.into(),
        }
    }

    /// Decode the raw bytes from an OBlob, reversing the base64 encoding.
    /// Returns None if called on a non-Blob variant.
    pub fn blob_bytes(&self) -> Option<Vec<u8>> {
        match self {
            OValue::Blob { v, .. } => B64.decode(v).ok(),
            _                      => None,
        }
    }

    /// Returns the MIME type of a Blob variant. None for all other variants.
    pub fn blob_mime(&self) -> Option<&str> {
        match self {
            OValue::Blob { mime, .. } => Some(mime.as_str()),
            _                         => None,
        }
    }
}


// ═════════════════════════════════════════════════════════════════════════════
// SECTION 3: Type predicates
// ═════════════════════════════════════════════════════════════════════════════

impl OValue {
    pub fn is_null(&self)  -> bool { matches!(self, OValue::Null) }
    pub fn is_bool(&self)  -> bool { matches!(self, OValue::Bool  { .. }) }
    pub fn is_int(&self)   -> bool { matches!(self, OValue::Int   { .. }) }
    pub fn is_float(&self) -> bool { matches!(self, OValue::Float { .. }) }
    pub fn is_str(&self)   -> bool { matches!(self, OValue::Str   { .. }) }
    pub fn is_html(&self) -> bool { matches!(self, OValue::Html { .. }) }
    pub fn is_store_path(&self) -> bool { matches!(self, OValue::StorePath { .. }) }
    pub fn is_list(&self)  -> bool { matches!(self, OValue::List  { .. }) }
    pub fn is_map(&self)   -> bool { matches!(self, OValue::Map   { .. }) }
    pub fn is_blob(&self)  -> bool { matches!(self, OValue::Blob  { .. }) }
    pub fn is_nix_expr(&self) -> bool { matches!(self, OValue::NixExpr { .. }) }
    pub fn is_numeric(&self) -> bool { self.is_int() || self.is_float() }

    /// The type name as it appears in the wire protocol `t` field.
    pub fn type_name(&self) -> &'static str {
        match self {
            OValue::Null      => "null",
            OValue::Bool  {..} => "bool",
            OValue::Int   {..} => "int",
            OValue::Float {..} => "float",
            OValue::Str   {..} => "str",
            OValue::Html  { .. } => "html",
            OValue::StorePath { .. } => "store_path",
            OValue::List  {..} => "list",
            OValue::Map   {..} => "map",
            OValue::Blob  {..} => "blob",
            OValue::NixExpr {..} => "nix_expr",
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
            OValue::Int   { v } => Ok(*v as f64),
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
            OValue::Null          => "null".to_string(),
            OValue::Bool  { v }   => v.to_string(),
            OValue::Int   { v }   => v.to_string(),
            OValue::Float { v }   => {
                // Always include decimal point — "3" vs "3.0" matters in some langs
                if v.fract() == 0.0 { format!("{:.1}", v) }
                else                 { v.to_string() }
            },
            OValue::Str   { v }   => v.clone(),
            OValue::Html { v } => v.clone(),
            OValue::StorePath { path } => path.clone(),
            OValue::List  { v }   => {
                let items: Vec<String> = v.iter().map(|i| i.splice_repr()).collect();
                format!("[{}]", items.join(", "))
            },
            OValue::Map   { v }   => {
                let pairs: Vec<String> = v.iter()
                    .map(|(k, val)| format!("{:?}: {}", k, val.splice_repr()))
                    .collect();
                format!("{{{}}}", pairs.join(", "))
            },
            OValue::Blob  { v, mime } => {
                format!("data:{};base64,{}", mime, v)
            },
            // ONixExpr splices as the raw Nix body — the expression is already
            // valid Nix source text that can be embedded directly in a Nix context.
            OValue::NixExpr { body, .. } => body.clone(),
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
            OValue::Null          => write!(f, "null"),
            OValue::Bool  { v }   => write!(f, "{}", v),
            OValue::Int   { v }   => write!(f, "{}", v),
            OValue::Float { v }   => write!(f, "{}", v),
            OValue::Str   { v }   => write!(f, "{:?}", v),
            OValue::List  { v }   => {
                write!(f, "[")?;
                for (i, item) in v.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", item)?;
                }
                write!(f, "]")
            },
            OValue::Map   { v }   => {
                write!(f, "{{")?;
                for (i, (k, val)) in v.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{:?}: {}", k, val)?;
                }
                write!(f, "}}")
            },
            OValue::Html { v } => write!(f, "{}", v),
            OValue::StorePath { path } => write!(f, "{}", path),
            OValue::Blob  { mime, .. } => write!(f, "<blob:{}>", mime),
            OValue::NixExpr { fingerprint, deps, .. } => {
                write!(f, "<nix_expr fp={} deps={}>", &fingerprint[..8], deps.len())
            },
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
        code:     String,
        bindings: HashMap<String, OValue>,
    },

    /// Clear the backend's environment and release all resources.
    /// Sent when a persistent env [n] is garbage collected, or on shutdown.
    Cleanup,

    /// Verify the backend process is alive and responsive.
    /// Used by the process manager before sending real work.
    Ping,
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
}

impl OWireResponse {
    pub fn ok(value: OValue) -> Self {
        OWireResponse::Ok { value }
    }

    pub fn err(message: impl Into<String>) -> Self {
        OWireResponse::Err { message: message.into() }
    }

    pub fn into_result(self) -> Result<OValue> {
        match self {
            OWireResponse::Ok  { value }   => Ok(value),
            OWireResponse::Err { message } => bail!("{}", message),
        }
    }
}


// ═════════════════════════════════════════════════════════════════════════════
// SECTION 8: Error types
// ═════════════════════════════════════════════════════════════════════════════

#[derive(thiserror::Error, Debug)]
pub enum OValueError {
    #[error("Type mismatch: expected {expected}, got {actual}")]
    TypeMismatch { expected: &'static str, actual: String },

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
            OValue::float(3.14159),
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
            OValue::blob(b"\x89PNG\r\n", "image/png"),
            OValue::blob(&[], "application/octet-stream"),
        ];

        for original in &cases {
            let json    = serde_json::to_string(original)
                .unwrap_or_else(|e| panic!("Serialize failed for {:?}: {}", original, e));
            let decoded: OValue = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("Deserialize failed for {}: {}", json, e));
            assert_eq!(*original, decoded,
                "Round-trip failed: {:?} → {} → {:?}", original, json, decoded);
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
        let json    = serde_json::to_string(&cmd).unwrap();
        let decoded: OWireCommand = serde_json::from_str(&json).unwrap();
        // Verify the cmd tag is present and correct
        assert!(json.contains(r#""cmd":"exec""#));

        let resp = OWireResponse::ok(OValue::str_("result"));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""status":"ok""#));
        let decoded: OWireResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, OWireResponse::Ok { .. }));
    }

    /// The type_name method must return the exact string used as the
    /// wire protocol `t` tag — they must stay in sync.
    #[test]
    fn type_names_match_wire_tags() {
        let cases = vec![
            (OValue::null(),          "null"),
            (OValue::bool_(true),     "bool"),
            (OValue::int(0),          "int"),
            (OValue::float(0.0),      "float"),
            (OValue::str_(""),        "str"),
            (OValue::list(vec![]),    "list"),
            (OValue::map(HashMap::new()), "map"),
            (OValue::blob(&[], ""),   "blob"),
        ];
        for (val, expected_tag) in cases {
            assert_eq!(val.type_name(), expected_tag);
            let json: serde_json::Value = serde_json::from_str(
                &serde_json::to_string(&val).unwrap()
            ).unwrap();
            assert_eq!(json["t"].as_str().unwrap(), expected_tag,
                "Wire tag mismatch for {}", expected_tag);
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
        assert_eq!(OValue::null().splice_repr(),       "null");
        assert_eq!(OValue::bool_(true).splice_repr(),  "true");
        assert_eq!(OValue::int(42).splice_repr(),      "42");
        assert_eq!(OValue::float(3.0).splice_repr(),   "3.0");
        assert_eq!(OValue::str_("hi").splice_repr(),   "hi");
    }

    /// ONixExpr constructor must compute a stable sha256(body) fingerprint,
    /// store deps by reference, and round-trip through JSON without loss.
    #[test]
    fn nix_expr_fingerprint_is_sha256_of_body() {
        let body  = "pkgs.hello";
        let val   = OValue::nix_expr(body, vec![]);

        if let OValue::NixExpr { fingerprint, .. } = &val {
            // sha256("pkgs.hello") = 6b0fc1cf4a0e73a498b0bd6b0d0e6ab91e01bc59…
            // Just verify it is a 64-hex-character string (256 bits).
            assert_eq!(fingerprint.len(), 64,
                "fingerprint should be 64 hex chars (sha256), got {:?}", fingerprint);
            assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()),
                "fingerprint must be hex, got {:?}", fingerprint);
        } else {
            panic!("expected OValue::NixExpr, got {:?}", val);
        }
    }

    #[test]
    fn nix_expr_same_body_produces_same_fingerprint() {
        let a = OValue::nix_expr("pkgs.hello", vec![]);
        let b = OValue::nix_expr("pkgs.hello", vec![]);
        if let (OValue::NixExpr { fingerprint: fa, .. }, OValue::NixExpr { fingerprint: fb, .. }) = (&a, &b) {
            assert_eq!(fa, fb, "identical bodies must produce identical fingerprints");
        }
    }

    #[test]
    fn nix_expr_different_body_produces_different_fingerprint() {
        let a = OValue::nix_expr("pkgs.hello", vec![]);
        let b = OValue::nix_expr("pkgs.world", vec![]);
        if let (OValue::NixExpr { fingerprint: fa, .. }, OValue::NixExpr { fingerprint: fb, .. }) = (&a, &b) {
            assert_ne!(fa, fb, "different bodies must produce different fingerprints");
        }
    }

    #[test]
    fn nix_expr_deps_are_stored_by_reference() {
        let dep   = OValue::str_("a_dep");
        let val   = OValue::nix_expr("some expr", vec![dep.clone()]);
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
        let json     = serde_json::to_string(&original).unwrap();
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
        let val  = OValue::nix_expr(body, vec![]);
        assert_eq!(val.splice_repr(), body);
    }
}
