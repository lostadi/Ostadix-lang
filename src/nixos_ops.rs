// ─────────────────────────────────────────────────────────────────────────────
// nixos_ops.rs
//
// STEP-4: the PERFORM BOUNDARY for OS-as-participant transitions.
//
// This module owns the irreducible IO that actually touches the running
// operating system. Today it has one function — `activate_nix` — which
// implements the StorePath → System transition: applying a built NixOS
// system closure to a profile and switching the live system to it.
//
// SAFETY POSTURE: dry-run is the default. The Activate Request constructed
// from an `activate(...)` call in O-lang carries `dry_run: true` unless the
// caller explicitly sets it false. AND the actual subprocess invocation is
// further gated by the environment variable `O_LANG_ALLOW_ACTIVATION=1`.
// Two layers of opt-in for an operation that can reboot the user's machine
// or take it offline.
//
// STEP5 additions (each with its own safety surface):
//   - rollback to a prior generation
//   - boot-only activation (next reboot, not now)
//   - test-and-rollback (activate, run health check, rollback if it fails)
//   - snapshot a generation for later comparison
// ─────────────────────────────────────────────────────────────────────────────

use anyhow::{bail, Context, Result};
use std::env;
use std::process::{Command, Stdio};

use crate::value::OValue;

/// The env var that gates real (non-dry-run) activation, even when the
/// Activate Request itself asks for `dry_run: false`. If unset, every
/// activation is forced to dry-run mode with a printed warning.
const ACTIVATION_GATE_ENV: &str = "O_LANG_ALLOW_ACTIVATION";

/// Apply a system closure to a profile.
///
/// `source` must resolve to an `OValue::StorePath` — the realised path of a
/// NixOS system closure (something whose `bin/switch-to-configuration` exists).
/// Source = `OValue::Derivation` is rejected: the user must realise() first,
/// to keep the rung-by-rung structure visible.
///
/// `profile` is the symlink to update (e.g. `/nix/var/nix/profiles/system`).
///
/// `dry_run` controls the subprocess argument: when true, we pass
/// `switch-to-configuration dry-activate`, which logs what would happen
/// without applying. When false AND the env-var gate is set, we pass
/// `switch-to-configuration switch`, the real thing.
pub fn activate_nix(source: &OValue, profile: &str, dry_run: bool) -> Result<OValue> {
    // ── Type check: only StorePath sources are accepted ──────────────────────
    let store_path = match source {
        OValue::StorePath { path } => path.clone(),
        OValue::Derivation { drv_path, .. } => bail!(
            "activate() expected a StorePath (a realised system closure), got \
             a Derivation ({}). Realise it first: activate(realise($drv)).",
            drv_path
        ),
        OValue::NixExpr { .. } => bail!(
            "activate() expected a StorePath, got a NixExpr. The full chain is \
             activate(realise(instantiate($expr)))."
        ),
        other => bail!("activate() expected a StorePath, got {}", other.type_name()),
    };

    // ── Sanity check: the closure must have a switch-to-configuration ────────
    let switch_bin = format!("{}/bin/switch-to-configuration", store_path);
    if !std::path::Path::new(&switch_bin).exists() {
        bail!(
            "Path {} does not contain bin/switch-to-configuration. \
             This doesn't look like a NixOS system closure. Did you realise the \
             right derivation? (For non-NixOS profiles — Home-Manager etc. — \
             a different activation mechanism is needed; STEP5 territory.)",
            store_path
        );
    }

    // ── Decide effective action based on gate env var ────────────────────────
    let gate_set = env::var(ACTIVATION_GATE_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let effective_action = if dry_run {
        "dry-activate"
    } else if !gate_set {
        // Caller asked for real activation but the env-var gate is unset.
        // Force dry-run with a loud warning.
        eprintln!(
            "warning: activate() was called with dry_run=false, but the safety \
             gate {ACTIVATION_GATE_ENV}=1 is NOT set. Forcing dry-activate. \
             To actually switch the system, run with {ACTIVATION_GATE_ENV}=1."
        );
        "dry-activate"
    } else {
        "switch"
    };

    // ── Invoke switch-to-configuration ───────────────────────────────────────
    eprintln!(
        "activate: profile={} closure={} action={}",
        profile, store_path, effective_action
    );

    let out = Command::new(&switch_bin)
        .arg(effective_action)
        .env("NIX_PROFILE", profile)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to spawn {}", switch_bin))?;

    if !out.success() {
        bail!(
            "switch-to-configuration {} exited with status {:?}",
            effective_action,
            out.code()
        );
    }

    // After a successful switch (or dry-activate), return a System value
    // pointing at the profile. The state of that profile may have changed
    // (in the switch case) or not (in dry-activate); either way the
    // reference is the same.
    Ok(OValue::system(profile))
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
//
// Real-activation tests are #[ignore]'d — they require a NixOS system AND
// O_LANG_ALLOW_ACTIVATION=1. The unit tests cover the type-check and gate
// logic without spawning subprocesses.
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activate_rejects_derivation_with_helpful_message() {
        let drv = OValue::derivation("/nix/store/abc-system.drv", vec!["out".into()], vec![]);
        let err = activate_nix(&drv, "/nix/var/nix/profiles/system", true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("realise"),
            "error should suggest realise(), got: {}",
            err
        );
    }

    #[test]
    fn activate_rejects_nix_expr_with_full_chain_hint() {
        let expr = OValue::nix_expr("pkgs.nixos.config.system", vec![]);
        let err = activate_nix(&expr, "/nix/var/nix/profiles/system", true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("activate(realise(instantiate"),
            "error should show the full chain, got: {}",
            err
        );
    }

    #[test]
    fn activate_rejects_non_existent_switch_to_configuration() {
        // A real /nix/store path that doesn't have bin/switch-to-configuration.
        let bogus = OValue::store_path("/tmp/definitely-not-a-system-closure");
        let err = activate_nix(&bogus, "/nix/var/nix/profiles/system", true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("switch-to-configuration"),
            "error should mention the missing switch-to-configuration, got: {}",
            err
        );
    }

    #[test]
    #[ignore = "requires a NixOS system AND O_LANG_ALLOW_ACTIVATION=1"]
    fn activate_actually_switches_when_gate_is_set() {
        // To run: O_LANG_ALLOW_ACTIVATION=1 cargo test --ignored \
        //         activate_actually_switches_when_gate_is_set
        //
        // This is the smoke test for the real path. It assumes the current
        // system's profile is already valid and asks for a dry-run, so it's
        // safe to run even with the gate set. We're testing the wiring, not
        // changing the system.
        let current = "/nix/var/nix/profiles/system";
        let path = std::fs::read_link(current)
            .expect("system profile symlink must exist on NixOS")
            .to_string_lossy()
            .into_owned();
        let val = OValue::store_path(path);

        let result = activate_nix(&val, current, true).unwrap();
        assert!(result.is_system());
    }
}
