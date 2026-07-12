//! Graph-execution coordinator.
//!
//! This module turns the projected operation hypergraph into the runtime's
//! default execution engine. The [`coordinator::Coordinator`] drives a
//! readiness-based event loop over operation hyperedges: an operation runs once
//! its data/structural/actor predecessors have committed, independent work runs
//! concurrently where provably safe, and results are committed in the plan's
//! deterministic root order. The reference `execute_plan_serial` executor
//! remains available behind `O_EXECUTOR=serial` for cross-checking.

pub mod actor;
pub mod cancellation;
pub mod coordinator;
pub mod effects;
pub mod parallel;
pub mod trace;

pub use actor::{ActorKey, ActorTable};
pub use cancellation::CancellationToken;
pub use coordinator::Coordinator;
pub use effects::{DeclaredPurity, EffectDeclaration, EffectSummary, ResourceKey};
