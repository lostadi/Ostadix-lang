//! Versioned compile-time source inventory for generated Ostadix runtimes.
//!
//! This lives with the engine so a packaged compiler consumes engine-owned
//! bytes through its dependency instead of reaching into a workspace sibling.

// ─────────────────────────────────────────────────────────────────────────────
// Runtime source files — embedded at olangc's own compile time.
//
// These are written verbatim into the temp project so the generated binary
// gets an identical copy of the Ostadix-lang runtime.  When the runtime changes,
// olangc must be recompiled for those changes to appear in newly compiled
// .O programs.
// ─────────────────────────────────────────────────────────────────────────────

pub const RUNTIME_VALUE_RS: &str = include_str!("../value.rs");
pub const RUNTIME_CAPABILITY_RS: &str = include_str!("../capability.rs");
pub const RUNTIME_ENVIRONMENT_RS: &str = include_str!("../environment.rs");
pub const RUNTIME_PARSER_RS: &str = include_str!("../parser.rs");
pub const RUNTIME_IR_RS: &str = include_str!("../ir.rs");
pub const RUNTIME_BACKEND_CATALOG_MODULE_RS: &str = include_str!("../backend_catalog.rs");
pub const RUNTIME_BACKEND_CATALOG_DATA_RS: &str = include_str!("../backend_catalog.inc.rs");
pub const RUNTIME_EXECUTION_CONTRACT_RS: &str = include_str!("../execution_contract.rs");
pub const RUNTIME_EVAL_CORE_RS: &str = include_str!("../eval_core.rs");
pub const RUNTIME_EVAL_RS: &str = include_str!("../eval.rs");
pub const RUNTIME_PROCESS_RS: &str = include_str!("../process.rs");
pub const RUNTIME_BACKEND_RS: &str = include_str!("../backend.rs");
pub const RUNTIME_BACKEND_MORPHISM_RS: &str = include_str!("../backend_morphism.rs");
pub const RUNTIME_BACKEND_STATE_RS: &str = include_str!("../backend_state.rs");
pub const RUNTIME_NIX_OPS_RS: &str = include_str!("../nix_ops.rs");
pub const RUNTIME_NIXOS_OPS_RS: &str = include_str!("../nixos_ops.rs");
pub const RUNTIME_SCHEDULER_RS: &str = include_str!("../scheduler.rs");
pub const RUNTIME_CANONICAL_CBOR_RS: &str = include_str!("../canonical_cbor.rs");
pub const RUNTIME_DISPATCH_MODEL_RS: &str = include_str!("../dispatch_model.rs");
pub const RUNTIME_SYNTAX_DIALECT_RS: &str = include_str!("../syntax_dialect.rs");
pub const RUNTIME_WIRE_RS: &str = include_str!("../wire.rs");
pub const RUNTIME_EFFECTS_RS: &str = include_str!("../effects.rs");
pub const RUNTIME_RUNTIME_EXEC_RS: &str = include_str!("../runtime_exec.rs");

// placement protocol + compiled catalog — the canonical identity, state,
// quota, and backend-capability vocabulary. Generated runtimes load the
// physical protocol tree once as `placement_protocol`; `placement` remains the
// public flat/nested compatibility projection over that same module identity.
pub const RUNTIME_PLACEMENT_SOURCES: &[(&str, &str)] = &[
    ("mod.rs", include_str!("../placement/mod.rs")),
    ("projection.rs", include_str!("../placement/projection.rs")),
    (
        "protocol/mod.rs",
        include_str!("../placement/protocol/mod.rs"),
    ),
    (
        "protocol/candidate.rs",
        include_str!("../placement/protocol/candidate.rs"),
    ),
    (
        "protocol/catalog.rs",
        include_str!("../placement/protocol/catalog.rs"),
    ),
    (
        "protocol/digest.rs",
        include_str!("../placement/protocol/digest.rs"),
    ),
    (
        "protocol/error.rs",
        include_str!("../placement/protocol/error.rs"),
    ),
    (
        "protocol/records.rs",
        include_str!("../placement/protocol/records.rs"),
    ),
    (
        "protocol/requirement.rs",
        include_str!("../placement/protocol/requirement.rs"),
    ),
    (
        "protocol/state.rs",
        include_str!("../placement/protocol/state.rs"),
    ),
    (
        "protocol/target.rs",
        include_str!("../placement/protocol/target.rs"),
    ),
    (
        "protocol/warrant.rs",
        include_str!("../placement/protocol/warrant.rs"),
    ),
];
pub const RUNTIME_REGISTRY_BUNDLE_RS: &str = include_str!("../registry/bundle/mod.rs");
pub const RUNTIME_REGISTRY_PLACEMENT_COMPAT_RS: &str =
    include_str!("../registry/placement_compat.rs");

// evidence — pre-execution facts and the admission compiler. These modules
// are part of every generated runtime because eval.rs cannot construct a
// Coordinator without an AdmittedExecution.
pub const RUNTIME_EVIDENCE_MOD_RS: &str = include_str!("../evidence/mod.rs");
pub const RUNTIME_EVIDENCE_FACT_RS: &str = include_str!("../evidence/fact.rs");
pub const RUNTIME_EVIDENCE_ANALYZE_RS: &str = include_str!("../evidence/analyze.rs");
pub const RUNTIME_EVIDENCE_ADMIT_RS: &str = include_str!("../evidence/admit.rs");
pub const RUNTIME_EVIDENCE_INTENT_RS: &str = include_str!("../evidence/intent.rs");
pub const RUNTIME_EVIDENCE_PROFILE_RS: &str = include_str!("../evidence/profile.rs");

// world — shared governed identities and the non-authorizing grounding view.
pub const RUNTIME_WORLD_MOD_RS: &str = include_str!("../world/mod.rs");
pub const RUNTIME_WORLD_CODEC_RS: &str = include_str!("../world/codec.rs");
pub const RUNTIME_WORLD_IDENTITY_RS: &str = include_str!("../world/identity.rs");
pub const RUNTIME_WORLD_IDENTITY_WIRE_RS: &str = include_str!("../world/identity_wire.rs");
pub const RUNTIME_WORLD_GROUNDING_RS: &str = include_str!("../world/grounding.rs");
pub const RUNTIME_WORLD_PROTOCOL_RS: &str = include_str!("../world/protocol.rs");
pub const RUNTIME_WORLD_RECEIPT_RS: &str = include_str!("../world/receipt.rs");
pub const RUNTIME_WORLD_RECEIPT_CODEC_RS: &str = include_str!("../world/receipt_codec.rs");
pub const RUNTIME_WORLD_VALUE_RS: &str = include_str!("../world/value.rs");
pub const RUNTIME_WORLD_VALUE_CODEC_RS: &str = include_str!("../world/value_codec.rs");

// hgraph — hypergraph substrate used by ir.rs and eval.rs at runtime.
pub const RUNTIME_HGRAPH_MOD_RS: &str = include_str!("../hgraph/mod.rs");
pub const RUNTIME_HGRAPH_GRAPH_RS: &str = include_str!("../hgraph/graph.rs");
pub const RUNTIME_HGRAPH_KINDS_RS: &str = include_str!("../hgraph/kinds.rs");
pub const RUNTIME_HGRAPH_FROM_OIR_RS: &str = include_str!("../hgraph/from_oir.rs");
pub const RUNTIME_HGRAPH_SCHEDULE_RS: &str = include_str!("../hgraph/schedule.rs");
pub const RUNTIME_HGRAPH_SOLVE_RS: &str = include_str!("../hgraph/solve.rs");

// executor: the readiness-driven graph coordinator used by eval.rs as the
// default execution engine, with its serial reference path retained in eval.rs.
pub const RUNTIME_EXECUTOR_MOD_RS: &str = include_str!("../executor/mod.rs");
pub const RUNTIME_EXECUTOR_ACTOR_RS: &str = include_str!("../executor/actor.rs");
pub const RUNTIME_EXECUTOR_CANCELLATION_RS: &str = include_str!("../executor/cancellation.rs");
pub const RUNTIME_EXECUTOR_COORDINATOR_RS: &str = include_str!("../executor/coordinator.rs");
pub const RUNTIME_EXECUTOR_DRIVER_RS: &str = include_str!("../executor/driver.rs");
pub const RUNTIME_EXECUTOR_EFFECTS_RS: &str = include_str!("../executor/effects.rs");
pub const RUNTIME_EXECUTOR_PARALLEL_RS: &str = include_str!("../executor/parallel.rs");
pub const RUNTIME_EXECUTOR_POOL_RS: &str = include_str!("../executor/pool.rs");
pub const RUNTIME_EXECUTOR_TASK_RS: &str = include_str!("../executor/task.rs");
pub const RUNTIME_EXECUTOR_TRACE_RS: &str = include_str!("../executor/trace.rs");

// project — first-class project/route/bundle model, embedded so compiled
// project binaries can materialize and run their embedded routes.
pub const RUNTIME_PROJECT_SOURCES: &[(&str, &str)] = &[
    ("mod.rs", include_str!("../project/mod.rs")),
    ("bundle.rs", include_str!("../project/bundle.rs")),
    ("deployment.rs", include_str!("../project/deployment.rs")),
    ("discover.rs", include_str!("../project/discover.rs")),
    ("executor.rs", include_str!("../project/executor.rs")),
    ("launch.rs", include_str!("../project/launch.rs")),
    ("logical.rs", include_str!("../project/logical.rs")),
    ("lower.rs", include_str!("../project/lower.rs")),
    ("manifest.rs", include_str!("../project/manifest.rs")),
    ("materialize.rs", include_str!("../project/materialize.rs")),
    ("model.rs", include_str!("../project/model.rs")),
    ("plan.rs", include_str!("../project/plan.rs")),
    ("runtime.rs", include_str!("../project/runtime.rs")),
    (
        "runtime_graph.rs",
        include_str!("../project/runtime_graph.rs"),
    ),
    ("trace.rs", include_str!("../project/trace.rs")),
    (
        "world_execution.rs",
        include_str!("../project/world_execution.rs"),
    ),
    (
        "ecosystems/mod.rs",
        include_str!("../project/ecosystems/mod.rs"),
    ),
    (
        "ecosystems/c_family.rs",
        include_str!("../project/ecosystems/c_family.rs"),
    ),
    (
        "ecosystems/dotnet.rs",
        include_str!("../project/ecosystems/dotnet.rs"),
    ),
    (
        "ecosystems/generic.rs",
        include_str!("../project/ecosystems/generic.rs"),
    ),
    (
        "ecosystems/haskell_ocaml.rs",
        include_str!("../project/ecosystems/haskell_ocaml.rs"),
    ),
    (
        "ecosystems/java.rs",
        include_str!("../project/ecosystems/java.rs"),
    ),
    (
        "ecosystems/javascript.rs",
        include_str!("../project/ecosystems/javascript.rs"),
    ),
    (
        "ecosystems/nix.rs",
        include_str!("../project/ecosystems/nix.rs"),
    ),
    (
        "ecosystems/python.rs",
        include_str!("../project/ecosystems/python.rs"),
    ),
    (
        "ecosystems/rust.rs",
        include_str!("../project/ecosystems/rust.rs"),
    ),
    (
        "ecosystems/shell.rs",
        include_str!("../project/ecosystems/shell.rs"),
    ),
];
