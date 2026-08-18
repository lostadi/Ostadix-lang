//! Compatibility library surface for the `o-lang` package.
//!
//! The runtime implementation lives in the independently packageable
//! `ostadix-api` engine.  This crate deliberately re-exports the historical
//! module paths used by the root CLI and downstream `o_lang` embedders.

pub use ostadix_api::{
    api, backend, backend_morphism, backend_state, effects, environment, eval, evidence,
    execution_contract, executor, hgraph, hosted_remote, information, information_bridge,
    information_provenance, ir, kernel_world, live_system, nix_ops, nixos_ops, ocore, parser,
    placement, process, project, registry, resource_identity, runtime_exec, scheduler, shims,
    syntax_dialect, value, version, wire, world,
};
