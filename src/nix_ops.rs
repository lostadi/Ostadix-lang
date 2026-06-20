// ─────────────────────────────────────────────────────────────────────────────
// nix_ops.rs
//
// The PERFORM BOUNDARY for the Nix rung lattice.
//
// This module wraps the actual `nix` subprocess calls that climb the lattice:
//   - instantiate_nix:  NixExpr → Derivation       (calls `nix eval ... drvPath`)
//   - realise_nix:      Derivation → StorePath     (calls `nix build`)
//
// Everything above this module (Request[Climb], the Executor trait,
// ImmediateExecutor in eval.rs) is policy and orchestration. Everything in
// this module is the irreducible IO that actually touches the Nix store.
//
// Step-2 simplifications (each marked with STEP3 for future expansion):
//   - We use the modern `nix eval` / `nix build` commands rather than the
//     classical `nix-instantiate` / `nix-store --realise`. Lee's pick;
//     uniform with the existing nix_backend / nix_store_backend.
//   - realise_nix returns ONLY the `out` output as an OStorePath. Multi-output
//     realise is STEP3.
//   - No retry, no concurrency, no progress reporting. Synchronous subprocess
//     each time. The scheduler in STEP3 will own these concerns.
// ─────────────────────────────────────────────────────────────────────────────

use anyhow::{anyhow, bail, Context, Result};
use std::process::{Command, Stdio};

use crate::value::OValue;

// ═════════════════════════════════════════════════════════════════════════════
// instantiate: NixExpr → Derivation
// ═════════════════════════════════════════════════════════════════════════════

/// Instantiate a NixExpr into a Derivation.
///
/// The Nix source body is wrapped in `(BODY).drvPath` and evaluated with
/// `nix eval --raw`. Then `nix derivation show <drv_path>` is parsed for the
/// list of output names.
///
/// Errors:
///   - `nix` executable not found              → RuntimeError with install hint
///   - the Nix body fails to evaluate          → bubble stderr
///   - the body does not evaluate to a drvPath → type error message
pub fn instantiate_nix(source: &OValue) -> Result<OValue> {
    let (body, deps) = match source {
        OValue::NixExpr { body, deps, .. } => (body.clone(), deps.clone()),
        other => bail!(
            "instantiate() expected a NixExpr (nix_expr^(...)_nix_expr block), got {}",
            other.type_name()
        ),
    };

    // ── Step 1: ask Nix for the .drv path of (body) ───────────────────────────
    //
    // The wrapper expression turns the user's body into a derivation reference
    // and asks for its drvPath. This works for `pkgs.hello`, for explicit
    // `derivation { ... }`, and for any expression whose value has a drvPath.
    let wrapper = format!("(let v = ({}); in v.drvPath)", body);

    let out = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "eval",
            "--raw",
            "--impure",
            "--expr",
            &wrapper,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| "failed to spawn `nix eval` — is Nix installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "nix eval failed while instantiating (exit {:?}):\nSTDERR:\n{}",
            out.status.code(),
            stderr
        );
    }

    let drv_path = String::from_utf8(out.stdout)
        .context("nix eval produced non-UTF-8 output")?
        .trim()
        .to_string();

    if !drv_path.starts_with("/nix/store/") || !drv_path.ends_with(".drv") {
        bail!(
            "instantiate() expected the Nix expression to evaluate to a derivation \
             (its .drvPath should be a /nix/store/*.drv path), got: {:?}",
            drv_path
        );
    }

    // ── Step 2: enumerate outputs via `nix derivation show` ──────────────────
    //
    // The output is a JSON map keyed by drv path. We extract the `outputs`
    // dict's keys (the output names like "out", "dev", "lib").
    let show = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "derivation",
            "show",
            &drv_path,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| "failed to spawn `nix derivation show`")?;

    let outputs = if show.status.success() {
        parse_outputs_from_show(&show.stdout, &drv_path).unwrap_or_else(|_| vec!["out".into()])
    } else {
        // STEP3: surface this as a warning. Today we fall back silently — almost
        // all derivations have an "out" output, so this default is safe enough
        // for step 2.
        vec!["out".into()]
    };

    Ok(OValue::derivation(drv_path, outputs, deps))
}

/// Parse the outputs array from `nix derivation show` JSON.
///
/// The shape is:
///   { "/nix/store/<hash>-<name>.drv": { "outputs": { "out": {...}, "dev": {...} } } }
///
/// We return the keys of the inner `outputs` map.
fn parse_outputs_from_show(json_bytes: &[u8], drv_path: &str) -> Result<Vec<String>> {
    let v: serde_json::Value =
        serde_json::from_slice(json_bytes).context("nix derivation show produced invalid JSON")?;

    let outputs_obj = v
        .get(drv_path)
        .and_then(|d| d.get("outputs"))
        .and_then(|o| o.as_object())
        .ok_or_else(|| {
            anyhow!(
                "nix derivation show JSON missing outputs map for {}",
                drv_path
            )
        })?;

    Ok(outputs_obj.keys().cloned().collect())
}

// ═════════════════════════════════════════════════════════════════════════════
// realise: Derivation → StorePath
// ═════════════════════════════════════════════════════════════════════════════

/// Realise a Derivation into a StorePath.
///
/// STEP-2 SIMPLIFICATION: only the `out` output is returned. Multi-output
/// realise is STEP3.
///
/// We invoke `nix build <drv_path>^out --no-link --print-out-paths`, which
/// builds the requested output and prints its store path. The `--no-link`
/// flag suppresses the `./result` symlink that nix-build would otherwise
/// create in the working directory.
pub fn realise_nix(source: &OValue) -> Result<OValue> {
    let drv_path = match source {
        OValue::Derivation { drv_path, .. } => drv_path.clone(),
        other => bail!(
            "realise() expected a Derivation (the output of instantiate()), got {}",
            other.type_name()
        ),
    };

    // The `^out` suffix on the drv reference asks Nix to build the `out` output.
    // STEP3: take an output name argument so callers can pick `dev`, `lib`, etc.
    let target = format!("{}^out", drv_path);

    let out = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "build",
            &target,
            "--no-link",
            "--print-out-paths",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| "failed to spawn `nix build`")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "nix build failed while realising {} (exit {:?}):\nSTDERR:\n{}",
            drv_path,
            out.status.code(),
            stderr
        );
    }

    let path = String::from_utf8(out.stdout)
        .context("nix build produced non-UTF-8 output")?
        .trim()
        .lines()
        .last()
        .ok_or_else(|| anyhow!("nix build returned no output paths"))?
        .to_string();

    if !path.starts_with("/nix/store/") {
        bail!(
            "realise() expected nix build to print a /nix/store/* path, got: {:?}",
            path
        );
    }

    Ok(OValue::store_path(path))
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
//
// These are integration-style — they only run if Nix is actually installed.
// They use #[ignore] so `cargo test` skips them by default; run them with
// `cargo test --ignored` on a machine with Nix.
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires Nix installation and network access"]
    fn instantiate_pkgs_hello_returns_derivation() {
        let body = "(import <nixpkgs> {}).hello";
        let expr = OValue::nix_expr(body, vec![]);
        let drv = instantiate_nix(&expr).expect("instantiation should succeed");
        assert!(drv.is_derivation());
        if let OValue::Derivation {
            drv_path, outputs, ..
        } = drv
        {
            assert!(drv_path.starts_with("/nix/store/"));
            assert!(drv_path.ends_with(".drv"));
            assert!(outputs.contains(&"out".to_string()));
        }
    }

    #[test]
    #[ignore = "requires Nix installation and network access"]
    fn realise_after_instantiate_returns_store_path() {
        let body = "(import <nixpkgs> {}).hello";
        let expr = OValue::nix_expr(body, vec![]);
        let drv = instantiate_nix(&expr).expect("instantiation should succeed");
        let path = realise_nix(&drv).expect("realisation should succeed");
        if let OValue::StorePath { path } = path {
            assert!(path.starts_with("/nix/store/"));
            assert!(
                !path.ends_with(".drv"),
                "realised path should not be a .drv"
            );
        } else {
            panic!("expected StorePath");
        }
    }

    #[test]
    fn instantiate_wrong_source_type_errors() {
        let not_an_expr = OValue::int(42);
        let err = instantiate_nix(&not_an_expr).unwrap_err();
        assert!(err.to_string().contains("NixExpr"));
    }

    #[test]
    fn realise_wrong_source_type_errors() {
        let not_a_drv = OValue::str_("/nix/store/something-not-a-drv");
        let err = realise_nix(&not_a_drv).unwrap_err();
        assert!(err.to_string().contains("Derivation"));
    }
}
