//! Graph-execution coordinator.
//!
//! This module turns the projected operation hypergraph into a runtime
//! execution engine. The [`coordinator::Coordinator`] drives a
//! readiness-based event loop over operation hyperedges: an operation runs once
//! its data/structural/actor predecessors have committed, independent work runs
//! concurrently where provably safe, and results are committed in the plan's
//! deterministic root order. During the state-complete refactor, the reference
//! serial executor remains the default and `O_EXECUTOR=graph` opts in.

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
