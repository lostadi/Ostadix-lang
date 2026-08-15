// ─────────────────────────────────────────────────────────────────────────────
// Ostadix-lang runtime library
//
// The historical 0.2 module surface remains available for compatibility with
// the interpreter, generated AOT crates, MCP, and existing embedders. New
// consumers should begin with `o_lang::api`; implementation modules will move
// behind that curated façade in later compatibility releases.
// ─────────────────────────────────────────────────────────────────────────────

pub mod api;
pub mod backend;
mod capability;
pub mod effects;
pub mod environment;
pub mod eval;
pub mod evidence;
pub mod executor;
pub mod hgraph;
pub mod hosted_remote;
pub mod ir;
pub mod kernel_world;
pub mod live_system;
pub mod nix_ops;
pub mod nixos_ops;
pub mod ocore;
pub mod parser;
pub mod placement;
pub mod process;
pub mod project;
pub mod registry;
pub mod runtime_exec;
pub mod scheduler;
pub mod shims;
pub mod value;
pub mod version;
pub mod wire;
pub mod world;
