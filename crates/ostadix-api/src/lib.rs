// ─────────────────────────────────────────────────────────────────────────────
// Ostadix independent runtime engine
//
// This crate owns the parser, IR, evaluator, admission compiler, scheduler,
// hosted execution machinery, values, and runtime assets. The root `o-lang`
// package is a compatibility and CLI shell over these exact type identities.
// ─────────────────────────────────────────────────────────────────────────────

pub mod api;
pub mod backend;
pub(crate) mod backend_catalog;
pub mod backend_morphism;
pub mod backend_state;
pub mod boot_objects;
mod canonical_cbor;
mod capability;
pub mod computation;
pub mod computation_core;
mod dispatch_model;
pub mod effects;
pub mod environment;
pub mod eval;
pub(crate) mod eval_core;
pub mod evidence;
pub mod execution_contract;
pub mod execution_fabric;
pub mod execution_fabric_authority;
pub mod executor;
pub mod hgraph;
pub mod hosted_remote;
pub mod information;
pub mod information_bridge;
pub mod information_provenance;
pub mod intent;
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

pub use api::{
    BackendAuthority, BigInt, CapabilityKind, DecimalSpecial, FloatFormat, FloatSpecial, GraphNode,
    GroupMode, NativeBoundary, NativeCodecSafety, NativeIdentity, NodeId, OBytes, OKeyword,
    ONative, ONumber, OSymbol, OText, OValue, RehydratePolicy, RequestKind, Runtime,
    RuntimeBoundary, RuntimeError, RuntimeStage, SeqKind, SetKind, SnapshotKind,
};
