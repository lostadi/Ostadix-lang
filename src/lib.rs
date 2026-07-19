// ─────────────────────────────────────────────────────────────────────────────
// Ostadix-lang runtime library
//
// All runtime modules are re-exported here so that both the `O` interpreter
// binary and `olangc`-compiled binaries share the same public API surface.
// Making these modules part of a library crate ensures every `pub` item is
// considered reachable (it's public API), eliminating dead-code warnings
// without suppression attributes.
// ─────────────────────────────────────────────────────────────────────────────

pub mod backend;
mod capability;
pub mod effects;
pub mod eval;
pub mod executor;
pub mod hgraph;
pub mod ir;
pub mod live_system;
pub mod nix_ops;
pub mod nixos_ops;
pub mod ocore;
pub mod parser;
pub mod process;
pub mod project;
pub mod scheduler;
pub mod shims;
pub mod value;
pub(crate) mod wire;
