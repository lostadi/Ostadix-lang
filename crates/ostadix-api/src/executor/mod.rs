//! Graph-execution coordinator.
//!
//! This module turns the projected operation hypergraph into a runtime
//! execution engine. The [`coordinator::Coordinator`] drives a
//! readiness-based event loop over operation hyperedges: an operation runs once
//! every ordinary/state/control input has materialized, independent work runs
//! concurrently where provably safe, and results are committed in the plan's
//! deterministic root order. The state-complete graph coordinator is the
//! default; `O_EXECUTOR=serial` selects the differential reference executor.

pub mod actor;
pub mod cancellation;
pub mod coordinator;
mod driver;
pub mod effects;
pub mod parallel;
pub mod pool;
pub mod task;
pub mod trace;

pub use actor::{ActorKey, ActorTable};
pub use cancellation::CancellationToken;
pub use coordinator::Coordinator;
pub(crate) use coordinator::GraphExecutorHost;
pub(crate) use driver::{AttemptDriver, PhysicalAttemptAdapterV1, PreparedPhysicalAttemptV1};
pub use effects::{
    effect_summary_for_plan_node, ActorResourceId, DeclaredPurity, EffectConfidence,
    EffectDeclaration, EffectSummary, EffectTrustPolicy, Fallibility, GovernedResourceKind,
    ResourceKey,
};
