use num_bigint::BigInt;

use crate::effects::ResourceKey;
use crate::ir::InvokeMode;
use crate::value::GroupMode;

use super::graph::ActorId;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct DomainFlags: u16 {
        const INTEGER  = 0x0001;
        const FLOAT    = 0x0002;
        const NUMERIC  = Self::INTEGER.bits() | Self::FLOAT.bits();
        const POINTER  = 0x0004;
        const BOOL     = 0x0008;
        const BITFIELD = 0x0010;
        const STRING   = 0x0020;
        const STRUCT   = 0x0040;
        const ANY      = 0x00ff;
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct RepFlags: u16 {
        const I8   = 0x0001;
        const I16  = 0x0002;
        const I32  = 0x0004;
        const I64  = 0x0008;
        const I128 = 0x0010;
        const BIG  = 0x0020;
        const F32  = 0x0040;
        const F64  = 0x0080;
        const PTR  = 0x0100;
        const BOOL = 0x0200;
        const STR  = 0x0400;
        const ANY  = 0x07ff;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpKind {
    // Type-bearing relations.
    Additive,
    Multiplicative,
    Bitwise,
    Ordered,
    Bounded { value: BigInt },
    AbiFixed { dom: DomainFlags, rep: RepFlags },
    Dereferenceable,
    FieldAccess { field: String },

    // Scheduling-bearing relations.
    DataFlow,
    StructuralBarrier,
    Sequence,
    ActorSerial { actor: ActorId },

    // First-class group topology.
    Batch,
    All,
    Any,
    Race,

    // First-class control topology projected from ExecutionPlan.
    Request { kind: String },
    Schedule { kind: String },
    CacheMemo { cacheable: bool },

    // Backend value crossing.
    BackendCrossing { from_lang: String, to_lang: String },

    // Native/lifted frontends.
    X86 { mnemonic: String },
    OcoreOp { kind: OcoreOpKind },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OcoreOpKind {
    Add,
    Sub,
    Mul,
    Div,
    Load,
    Store,
    Inb,
    Outb,
    VolatileLoad,
    VolatileStore,
    AtomicFetch { order: MemOrder },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemOrder {
    Relaxed,
    Acquire,
    Release,
    AcqRel,
    SeqCst,
}

// ─────────────────────────────────────────────────────────────────────────────
// Ontology: values are nodes, operations are hyperedges.
//
// Every hyperedge is either an executable operation that consumes input nodes
// and produces one or more value/resource/completion outputs (`Execute`), or a
// constraint relation over nodes that carries type/fidelity/scheduling facts
// (`Constraint`).
//
// The pre-existing typed-relation vocabulary lives in `OpKind` and is reached
// through `ConstraintOp::Type(OpKind)`. This keeps the type/fidelity solver in
// `solve.rs` and the DOT exporter in `olangc` operating over the same `OpKind`
// values they always did, while the executor reasons about the higher-level
// `HEdgeKind` classification.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HEdgeKind {
    /// An operation hyperedge: input nodes → one or more output nodes.
    Execute(ExecutableOp),
    /// A constraint relation over value nodes.
    Constraint(ConstraintOp),
}

/// The executable operations an operation hyperedge can carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutableOp {
    /// Bind an expression's value to a name (`let name = expr`). Produces a
    /// scope-delta value node the coordinator commits.
    Store,
    /// Read a binding from scope / a dominating store (`$name`).
    LoadBinding,
    /// Invoke an O-level builtin (`instantiate`, `now`, `scope`, …).
    Invoke {
        fn_name: String,
        mode: InvokeMode,
    },
    /// Execute a hosted backend block that reaches a shim / nix_expr / thunk.
    EvalBackend {
        lang: String,
        env: u32,
    },
    /// Assemble a pure inline backend body (`html`, `markdown`, `text`,
    /// `latex`, `quote`, `O`).
    InlineBackend {
        lang: String,
    },
    /// Force a request immediately (`now(req)`).
    ForceRequest {
        kind: String,
    },
    /// Construct a request value (`instantiate`, `realise`, `activate`, …).
    Request {
        kind: String,
    },
    /// Bundle members into a group (`batch`, `all`, `any`, `race`).
    Group {
        mode: GroupMode,
    },
    /// Scheduling control point (`lazy`, `autonomous`, `force`).
    Schedule {
        kind: String,
    },
    // ── Declared for project-lowering; not yet constructed by from_oir. ──
    MaterializeProject,
    BuildRoute {
        route_id: String,
    },
    RunRoute {
        route_id: String,
    },
    SelectRoute {
        policy: String,
    },
    CompareRouteResults,
}

/// The constraint relations a constraint hyperedge can carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConstraintOp {
    /// A value flows from a producer node to a consumer node.
    DataFlow,
    /// A structural child → parent evaluation dependency.
    Structural,
    /// Source sequence lowered as successful-completion control.
    SequenceControl,
    /// Compatibility relation for actor-oriented analysis. Production
    /// execution serializes persistent actors through `ActorState` resource
    /// transitions in executable-edge inputs and outputs.
    ActorSerial { actor: ActorId },
    /// Operations touching the same effectful resource must be ordered.
    EffectOrder { resource: ResourceKey },
    /// A non-blocking deterministic commit-order fact (stable ordinal).
    CommitOrder { ordinal: u64 },
    /// A backend value crossing from one language to another.
    BackendCrossing { from_lang: String, to_lang: String },
    /// A branch-guard predicate that must be active for the target to run.
    BranchGuard { guard: String },
    /// A typed/fidelity relation from the existing `OpKind` vocabulary.
    Type(OpKind),
}

impl ConstraintOp {
    /// Lift a legacy `OpKind` relation into the ontology. Scheduling/topology
    /// relations map to their dedicated constraint variants; everything else is
    /// wrapped as `Type(OpKind)` so the solver keeps its behavior.
    pub fn from_op_kind(kind: &OpKind) -> Self {
        match kind {
            OpKind::DataFlow => ConstraintOp::DataFlow,
            OpKind::StructuralBarrier => ConstraintOp::Structural,
            OpKind::Sequence => ConstraintOp::SequenceControl,
            OpKind::ActorSerial { actor } => ConstraintOp::ActorSerial { actor: *actor },
            OpKind::BackendCrossing { from_lang, to_lang } => ConstraintOp::BackendCrossing {
                from_lang: from_lang.clone(),
                to_lang: to_lang.clone(),
            },
            other => ConstraintOp::Type(other.clone()),
        }
    }
}

/// Materialization state of a value node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueState {
    /// No value has been produced yet.
    Unresolved,
    /// A value has been produced.
    Materialized,
    /// The producing operation failed.
    Failed(String),
    /// The node is on a branch that a guard disabled.
    DisabledByBranch,
}
