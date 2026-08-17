//! Canonical execution policy and lowered-metadata validation.
//!
//! This module owns the execution contract shared by evaluation, evidence,
//! admission, and World grounding. Runtime realization remains in `eval`; the
//! contract contains only stable policy vocabulary and pure validation of
//! already-lowered OIR metadata.

use std::collections::HashSet;

use anyhow::{bail, Context, Result};

use crate::backend_catalog::{BackendInterface, BackendRegistry, ExecutionMode};
use crate::effects::EffectDeclaration;
use crate::ir::{InvokeMode, OIr};
use crate::value::BackendAuthority;

// ═════════════════════════════════════════════════════════════════════════════
// Policy — WHEN does a Request execute?
//
// The "when" axis of the two-axis framing (the other is "who decides", which
// is the Executor). Step-3 ships Eager (default) and Lazy (scoped via lazy^).
// Autonomous is a placeholder for STEP4 — goal-driven scheduling, where the
// scheduler decides what to execute (and possibly speculatively pre-executes)
// based on goals carried alongside requests.
// ═════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Requests are auto-resolved (executed) at let-binding boundaries and
    /// at the top level. The user sees Derivations/StorePaths, never raw
    /// Requests. This is the default policy in eval_document.
    Eager,

    /// Requests pass through let-bindings as values. The user must explicitly
    /// call `now(req)` to perform a request. Entered via the `lazy^(...)_lazy`
    /// block — Policy::Lazy is in effect for the body of that block only,
    /// then restored to the surrounding policy on exit.
    Lazy,

    /// STEP-4: scheduler-directed buffered execution.
    ///
    /// Under Autonomous, non-Eval Requests are buffered as they're constructed
    /// instead of being executed immediately. At force points (exit of an
    /// `autonomous(expr)` block, explicit `now(req)`, document end), the
    /// AutonomousScheduler flushes the buffer: it collects the full transitive
    /// closure of all buffered requests, builds a dependency DAG, and dispatches
    /// independent requests as concurrent threads (up to `parallelism` at a
    /// time). Results are stored in a two-level cache (L1 memory + L2 disk).
    ///
    /// `RequestKind::Eval` is excluded from buffering — Eval needs the
    /// ProcessRegistry (which is !Send) and is executed eagerly even under
    /// this policy. Full Eval parallelism is a STEP5 goal.
    ///
    /// Activated via the `autonomous(expr)` built-in function, which evaluates
    /// `expr` under this policy, flushes the buffer on exit, and returns the
    /// resolved result.
    Autonomous,
}

impl Policy {
    /// Canonical stable spelling used by evidence, admission, and runtime
    /// context bindings.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Eager => "eager",
            Self::Lazy => "lazy",
            Self::Autonomous => "autonomous",
        }
    }

    /// Parse one canonical policy spelling without accepting aliases.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "eager" => Some(Self::Eager),
            "lazy" => Some(Self::Lazy),
            "autonomous" => Some(Self::Autonomous),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockEvalPolicy {
    Lazy,
    Defer,
}

#[derive(Debug, Default)]
pub(crate) struct BlockOptions {
    policy: Option<BlockEvalPolicy>,
    capability_binding: Option<String>,
    permissions: Vec<BackendAuthority>,
}

pub(crate) fn is_o_identifier(name: &str) -> bool {
    name.as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

impl BlockOptions {
    pub(crate) fn parse(attr: Option<&str>, lang: &str) -> Result<Self> {
        let mut options = Self::default();
        let mut seen = HashSet::new();
        EffectDeclaration::parse(attr)
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("invalid effect declaration on {lang}^"))?;
        for entry in attr.into_iter().flat_map(|attr| attr.split(',')) {
            let entry = entry.trim();
            if !seen.insert(entry.to_string()) {
                bail!("duplicate block attribute `{{{entry}}}` on {lang}^");
            }
            match entry {
                "lazy" => {
                    if options.policy.replace(BlockEvalPolicy::Lazy).is_some() {
                        bail!("a block cannot combine `lazy` and `defer`");
                    }
                }
                "defer" => {
                    if options.policy.replace(BlockEvalPolicy::Defer).is_some() {
                        bail!("a block cannot combine `lazy` and `defer`");
                    }
                }
                _ if entry.starts_with("cap=") => {
                    let name = entry.trim_start_matches("cap=");
                    if !is_o_identifier(name) {
                        bail!("backend capability binding `{name}` is not an O identifier");
                    }
                    if options.capability_binding.replace(name.into()).is_some() {
                        bail!("a block must name exactly one backend capability binding");
                    }
                }
                _ if EffectDeclaration::recognizes_entry(entry) => {}
                _ => {
                    let permission = BackendAuthority::parse(entry).ok_or_else(|| {
                        anyhow::anyhow!(
                            "unknown block attribute `{{{entry}}}` on {lang}^. Known attributes: lazy, defer, cap=name, fs_read, fs_write, network, process, effects=pure|unknown, reads=..., writes=..., serial=host"
                        )
                    })?;
                    options.permissions.push(permission);
                }
            }
        }
        options.permissions.sort();
        Ok(options)
    }

    pub(crate) const fn policy(&self) -> Option<BlockEvalPolicy> {
        self.policy
    }

    pub(crate) fn capability_binding(&self) -> Option<&str> {
        self.capability_binding.as_deref()
    }

    pub(crate) fn permissions(&self) -> &[BackendAuthority] {
        &self.permissions
    }
}

/// Validate the execution metadata embedded in lowered OIR before evidence is
/// issued. This is intentionally evaluator-independent: admission must reject
/// a forged backend interface or invocation contract instead of merely binding
/// the invalid metadata to a digest and deferring rejection until dispatch.
pub(crate) fn validate_execution_metadata(flat: &[&OIr]) -> Result<()> {
    for node in flat {
        match node {
            OIr::Invoke {
                fn_name,
                mode,
                args,
            } => {
                let canonical_mode = InvokeMode::for_name(fn_name);
                if *mode != canonical_mode {
                    bail!(
                        "OIR invocation `{fn_name}` uses mode {}, but canonical lowering requires {}",
                        mode.label(),
                        canonical_mode.label()
                    );
                }
                match mode {
                    InvokeMode::Lazy => {
                        if args.len() != 1 {
                            bail!("lazy(expr) takes exactly 1 argument, got {}", args.len());
                        }
                    }
                    InvokeMode::Autonomous => {
                        if args.len() != 1 {
                            bail!(
                                "autonomous(expr) takes exactly 1 argument, got {}",
                                args.len()
                            );
                        }
                    }
                    InvokeMode::Group(_) => {
                        if args.is_empty() {
                            bail!("{}(...) takes at least 1 argument, got 0", fn_name);
                        }
                    }
                    InvokeMode::Eager => {}
                }
            }
            OIr::Exec {
                lang,
                attr,
                backend,
                ..
            } => validate_exec_metadata(lang, attr.as_deref(), backend)?,
            _ => {}
        }
    }
    Ok(())
}

fn validate_exec_metadata(
    lang: &str,
    attr: Option<&str>,
    backend: &BackendInterface,
) -> Result<()> {
    let registered_backend = BackendRegistry::global().interface_for(lang);
    if backend != &registered_backend {
        bail!(
            "OIR backend interface for `{lang}` does not match the registered execution and authority policy"
        );
    }

    if backend.execution == ExecutionMode::InlineAst && backend.canonical == "quote" {
        if attr.is_some() {
            bail!("attributes are not valid on the structural `quote` backend");
        }
        return Ok(());
    }

    if backend.execution == ExecutionMode::InlineAst && backend.canonical == "O" {
        if attr.is_some() {
            bail!("attributes are not valid on the structural `O` backend");
        }
        return Ok(());
    }

    if backend.execution == ExecutionMode::InlineAst {
        bail!(
            "OIR backend `{}` declares inline_ast execution without an executor",
            backend.canonical
        );
    }

    let options = BlockOptions::parse(attr, lang)?;
    if let Some(policy) = options.policy() {
        match policy {
            BlockEvalPolicy::Lazy => {
                if lang == "nix_expr" {
                    bail!(
                        "`nix_expr{{lazy}}^` is redundant — nix_expr^ already \
                         captures its expression lazily. Use bare nix_expr^ for \
                         a captured Nix expression, or nix{{defer}}^ for a \
                         non-cacheable deferred raw Nix evaluation."
                    );
                }
                if !backend.pure {
                    bail!(
                        "`{lang}{{lazy}}^` is invalid because {lang} is not a \
                         pure backend; caching a thunk that re-runs with side \
                         effects would be unsound. Use `{lang}{{defer}}^` instead \
                         — it captures the same thunk but never caches and \
                         always re-runs on force.",
                        lang = lang
                    );
                }
            }
            BlockEvalPolicy::Defer => {
                if lang == "nix_expr" {
                    bail!(
                        "`nix_expr{{defer}}^` is redundant — nix_expr^ is already \
                         lazy. If you want a non-cacheable deferred Nix eval, \
                         write nix{{defer}}^."
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::any::TypeId;

    use super::Policy;

    #[test]
    fn policy_names_are_frozen_and_round_trip() {
        for (policy, name) in [
            (Policy::Eager, "eager"),
            (Policy::Lazy, "lazy"),
            (Policy::Autonomous, "autonomous"),
        ] {
            assert_eq!(policy.name(), name);
            assert_eq!(Policy::from_name(name), Some(policy));
        }
        for unsupported in ["Eager", "LAZY", "auto", "defer", ""] {
            assert_eq!(Policy::from_name(unsupported), None);
        }
    }

    #[test]
    fn compatibility_policy_paths_preserve_one_nominal_type_identity() {
        assert_eq!(TypeId::of::<Policy>(), TypeId::of::<crate::eval::Policy>());

        let compatibility: crate::eval::Policy = Policy::Eager;
        let _: Policy = compatibility;
    }
}
