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
pub(crate) mod backend_catalog;
pub mod backend_morphism;
pub mod backend_state;
mod canonical_cbor;
mod capability;
mod dispatch_model;
pub mod effects;
pub mod environment;
pub mod eval;
pub(crate) mod eval_core;
pub mod evidence;
pub mod execution_contract;
pub mod executor;
pub mod hgraph;
pub mod hosted_remote;
pub mod information;
pub mod ir;
pub mod kernel_world;
pub mod live_system;
pub mod nix_ops;
pub mod nixos_ops;
pub mod ocore;
pub mod parser;
pub mod placement;
#[path = "placement/protocol/mod.rs"]
pub(crate) mod placement_protocol;
pub mod process;
pub mod project;
pub mod registry;
#[path = "world/identity.rs"]
pub mod resource_identity;
pub mod runtime_exec;
pub mod scheduler;
pub mod shims;
pub mod syntax_dialect;
pub mod value;
pub mod version;
pub mod wire;
pub mod world;
