//! Lift a [`ProjectBundle`] into a single, valid `.O` document.
//!
//! The lifted program:
//!   * is parseable by the O-lang parser (validated by reparse),
//!   * embeds the full serialized bundle losslessly (base64 inside a
//!     `text^( … )_text` block), and
//!   * presents the route table in a readable comment block.
//!
//! File contents live only in the embedded bundle payload; execution goes
//! through routes, not by wrapping every file for sequential evaluation.

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};

use super::bundle::{deserialize, serialize};
use super::model::ProjectBundle;

/// Outer sentinel marking the start of the embedded project bundle.
pub const BUNDLE_BEGIN: &str = "# O-PROJECT-BUNDLE-V1 BEGIN";
/// Outer sentinel marking the end of the embedded project bundle.
pub const BUNDLE_END: &str = "# O-PROJECT-BUNDLE-V1 END";
/// Inner sentinel marking the start of the base64 payload lines.
const PAYLOAD_BEGIN: &str = "#olang-bundle-payload-begin";
/// Inner sentinel marking the end of the base64 payload lines.
const PAYLOAD_END: &str = "#olang-bundle-payload-end";

const CHUNK_WIDTH: usize = 76;

/// True when `source` contains an embedded project bundle.
pub fn has_embedded_bundle(source: &str) -> bool {
    source.contains(BUNDLE_BEGIN) && source.contains(PAYLOAD_BEGIN)
}

/// Lower a bundle into a valid `.O` document (infallible construction).
pub fn lower_to_o(bundle: &ProjectBundle) -> String {
    let payload = serialize(bundle).expect("bundle serialization must not fail");
    let encoded = B64.encode(&payload);

    let mut out = String::new();
    out.push_str("# Ostadix-lang lifted project\n");
    out.push_str(&format!("# project: {}\n", bundle.name));
    out.push_str(&format!(
        "# root_fingerprint: {}\n",
        bundle.root_fingerprint
    ));
    out.push_str(&format!(
        "# files: {}   routes: {}   route_sets: {}\n",
        bundle.files.len(),
        bundle.routes.len(),
        bundle.route_sets.len()
    ));
    out.push_str("#\n");
    out.push_str("# ── route table ──\n");
    for line in bundle.route_table().lines() {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("#\n");
    out.push_str("# The complete project (all files + routes) is embedded losslessly below.\n");
    out.push_str("# Extract it with the project tooling; run it through its routes.\n");
    out.push('\n');

    // ── Embedded bundle payload (base64 inside a text block) ─────────────────
    out.push_str(BUNDLE_BEGIN);
    out.push('\n');
    out.push_str("text^(\n");
    out.push_str(PAYLOAD_BEGIN);
    out.push('\n');
    for chunk in encoded.as_bytes().chunks(CHUNK_WIDTH) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        out.push('\n');
    }
    out.push_str(PAYLOAD_END);
    out.push('\n');
    out.push_str(")_text\n");
    out.push_str(BUNDLE_END);
    out.push('\n');
    out.push('\n');

    // Evaluating a lifted project directly must be both inert and intelligible.
    // The payload block above is inline text, not executable source, and this
    // final value replaces the otherwise enormous base64 payload as the CLI's
    // displayed document result.
    out.push_str("text^(\n");
    out.push_str("Ostadix project bundle loaded safely. No project route was executed.\n");
    out.push_str(
        "Use `o-link --list-routes <bundle.O>` and `o-link <bundle.O> --run --route <id>`.\n",
    );
    out.push_str(")_text\n");

    out
}

/// Lower a bundle and validate the result reparses as `.O` source.
pub fn lower_to_o_validated(bundle: &ProjectBundle) -> Result<String> {
    let source = lower_to_o(bundle);
    let backends = crate::backend_catalog::BackendRegistry::global().registered_backend_tags();
    let mut parser = crate::parser::Parser::new(&source, &backends);
    parser
        .parse()
        .context("lifted .O program does not reparse")?;

    // Round-trip guarantee: what we embedded must extract back byte-identically.
    let extracted = extract_bundle_from_o(&source)?;
    if &extracted != bundle {
        bail!("lifted .O program does not round-trip the project bundle");
    }
    Ok(source)
}

/// Extract the embedded [`ProjectBundle`] from a lifted `.O` document.
pub fn extract_bundle_from_o(source: &str) -> Result<ProjectBundle> {
    let mut lines = source.lines();
    // Advance to the payload begin marker.
    let mut found = false;
    for line in lines.by_ref() {
        if line.trim() == PAYLOAD_BEGIN {
            found = true;
            break;
        }
    }
    if !found {
        bail!("no embedded project bundle payload found");
    }

    let mut b64 = String::new();
    let mut closed = false;
    for line in lines {
        if line.trim() == PAYLOAD_END {
            closed = true;
            break;
        }
        for ch in line.chars() {
            if !ch.is_whitespace() {
                b64.push(ch);
            }
        }
    }
    if !closed {
        bail!("embedded project bundle payload is not terminated");
    }

    let bytes = B64
        .decode(b64.as_bytes())
        .context("embedded project bundle payload is not valid base64")?;
    deserialize(&bytes)
}
