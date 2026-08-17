// ─────────────────────────────────────────────────────────────────────────────
// olangc — the Ostadix-lang compiler
//
// Compiles a .O source file to a hosted native or WASI binary, executes its
// OIR directly in-process, prints that same executable OIR and plan, or emits
// the program's execution hypergraph as Graphviz DOT.
//
// Usage:
//   olangc <input.O>                              # binary target (default)
//   olangc <input.O> -o myprogram                 # explicit output name
//   olangc <input.O> --target wasm                # wasm32-wasip1
//   olangc <input.O> --target script              # run in-process
//   olangc <input.O> --target ir                  # dump the lowered OIR
//   olangc <input.O> --target ir --execution-intent-json
//   olangc <input.O> --target ir --explain-schedule --format json
//   olangc <input.O> --target ir --why P3         # explain one admitted operation
//   olangc <input.O> --target dot                 # Graphviz DOT hypergraph
//   olangc <input.O> --shim-dir ./backends        # custom shim directory
//
// Target A ("binary"):
//   1. Reads the .O source file.
//   2. Resolves compatibility backend adapters: starts from adapters that are
//      bundled into olangc itself at olangc's compile time (so olangc works
//      from any cwd with no adjacent backends/ directory), then optionally
//      overlays files from --shim-dir if the user passed one. Rust-native
//      backends do not need shim files.
//   3. Creates a temporary Cargo project that bundles:
//        - All Ostadix-lang runtime source files (embedded in olangc at its own
//          compile time via include_str!, so olangc is self-contained).
//        - The .O source file (copied as "program.O" in the generated src/).
//        - Compatibility adapter scripts (copied into src/shims/).
//        - A generated main.rs that references them via include_str!/include_bytes!.
//        - A Cargo.toml mirroring the runtime's dependencies.
//        - A Cargo.lock projected deterministically from the workspace lock to
//          the generated package's exact direct dependency set.
//        - The authoritative rust-toolchain.toml used to build this olangc.
//   4. Runs `cargo build --release --locked` in the temp project.
//   5. Copies the resulting binary to the requested output path.
//
//   The output binary is fully self-contained at the Rust level: it has no
//   dependency on the .O source file, the backends/ directory, or the olangc
//   tool itself. At runtime it still needs the language runtimes that the .O
//   program uses: Python for python^ blocks, Nix for nix^ blocks, etc.
//
// Target B ("wasm"):
//   Generates the same hosted runtime project for wasm32-wasip1.
//
// Target C ("script"):
//   Parses, lowers to OIR, validates ExecutionPlan, and executes the plan
//   directly inside the olangc process. No intermediate project or output
//   binary is produced.
//
// Target D ("ir"):
//   Parses the .O program, lowers the ONode forest to the OIR intermediate
//   representation (src/ir.rs), builds the canonical ExecutionPlan dependency
//   graph from that OIR, and prints both to stdout. This is the same OIR the
//   script and generated-binary runtimes execute.
//
// Target E ("dot"):
//   Parses and lowers to OIR, then builds the full hypergraph (src/hgraph/)
//   from that OIR, runs the type solver over it, and serialises the result as
//   a Graphviz DOT digraph on stdout. Ordinary values, resource/actor state,
//   and completion/control tokens use distinct node styles. Each directed
//   hyperedge is rendered as its own vertex, with arrows from every input port
//   into the vertex and arrows from the vertex to every output port. Execute
//   and constraint/type hyperedges are visually distinct. Pipe to
//   `dot -Tpng -o out.png` or any other Graphviz renderer.
// ─────────────────────────────────────────────────────────────────────────────

use anyhow::{bail, Context, Result};
use clap::{Parser as ClapParser, ValueEnum};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use o_lang::eval::Evaluator;
use o_lang::evidence::{
    admit_execution, analyze_execution, runtime_binding_from_adapter_bytes, ExecutionIntentV1,
};
use o_lang::execution_contract::Policy;
use o_lang::ir::{OIrProgram, PlanNodeId};
use o_lang::parser::Parser;
use o_lang::shims::read_shims;
use o_lang::value::OValue;
use o_lang::world::{GroundingReport, WorldEpoch, WorldId, WorldIdentity};

// ─────────────────────────────────────────────────────────────────────────────
// Runtime source files — embedded at olangc's own compile time.
//
// These are written verbatim into the temp project so the generated binary
// gets an identical copy of the Ostadix-lang runtime.  When the runtime changes,
// olangc must be recompiled for those changes to appear in newly compiled
// .O programs.
// ─────────────────────────────────────────────────────────────────────────────

const RUNTIME_VALUE_RS: &str = include_str!("../value.rs");
const RUNTIME_CAPABILITY_RS: &str = include_str!("../capability.rs");
const RUNTIME_ENVIRONMENT_RS: &str = include_str!("../environment.rs");
const RUNTIME_PARSER_RS: &str = include_str!("../parser.rs");
const RUNTIME_IR_RS: &str = include_str!("../ir.rs");
const RUNTIME_BACKEND_CATALOG_MODULE_RS: &str = include_str!("../backend_catalog.rs");
const RUNTIME_BACKEND_CATALOG_DATA_RS: &str = include_str!("../backend_catalog.inc.rs");
const RUNTIME_EXECUTION_CONTRACT_RS: &str = include_str!("../execution_contract.rs");
const RUNTIME_EVAL_CORE_RS: &str = include_str!("../eval_core.rs");
const RUNTIME_EVAL_RS: &str = include_str!("../eval.rs");
const RUNTIME_PROCESS_RS: &str = include_str!("../process.rs");
const RUNTIME_BACKEND_RS: &str = include_str!("../backend.rs");
const RUNTIME_BACKEND_MORPHISM_RS: &str = include_str!("../backend_morphism.rs");
const RUNTIME_BACKEND_STATE_RS: &str = include_str!("../backend_state.rs");
const RUNTIME_NIX_OPS_RS: &str = include_str!("../nix_ops.rs");
const RUNTIME_NIXOS_OPS_RS: &str = include_str!("../nixos_ops.rs");
const RUNTIME_SCHEDULER_RS: &str = include_str!("../scheduler.rs");
const RUNTIME_CANONICAL_CBOR_RS: &str = include_str!("../canonical_cbor.rs");
const RUNTIME_DISPATCH_MODEL_RS: &str = include_str!("../dispatch_model.rs");
const RUNTIME_SYNTAX_DIALECT_RS: &str = include_str!("../syntax_dialect.rs");
const RUNTIME_WIRE_RS: &str = include_str!("../wire.rs");
const RUNTIME_EFFECTS_RS: &str = include_str!("../effects.rs");
const RUNTIME_RUNTIME_EXEC_RS: &str = include_str!("../runtime_exec.rs");

// placement protocol + compiled catalog — the canonical identity, state,
// quota, and backend-capability vocabulary. Generated runtimes load the
// physical protocol tree once as `placement_protocol`; `placement` remains the
// public flat/nested compatibility projection over that same module identity.
const RUNTIME_PLACEMENT_SOURCES: &[(&str, &str)] = &[
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
const RUNTIME_REGISTRY_BUNDLE_RS: &str = include_str!("../registry/bundle/mod.rs");
const RUNTIME_REGISTRY_PLACEMENT_COMPAT_RS: &str = include_str!("../registry/placement_compat.rs");

// evidence — pre-execution facts and the admission compiler. These modules
// are part of every generated runtime because eval.rs cannot construct a
// Coordinator without an AdmittedExecution.
const RUNTIME_EVIDENCE_MOD_RS: &str = include_str!("../evidence/mod.rs");
const RUNTIME_EVIDENCE_FACT_RS: &str = include_str!("../evidence/fact.rs");
const RUNTIME_EVIDENCE_ANALYZE_RS: &str = include_str!("../evidence/analyze.rs");
const RUNTIME_EVIDENCE_ADMIT_RS: &str = include_str!("../evidence/admit.rs");
const RUNTIME_EVIDENCE_INTENT_RS: &str = include_str!("../evidence/intent.rs");
const RUNTIME_EVIDENCE_PROFILE_RS: &str = include_str!("../evidence/profile.rs");

// world — shared governed identities and the non-authorizing grounding view.
const RUNTIME_WORLD_MOD_RS: &str = include_str!("../world/mod.rs");
const RUNTIME_WORLD_CODEC_RS: &str = include_str!("../world/codec.rs");
const RUNTIME_WORLD_IDENTITY_RS: &str = include_str!("../world/identity.rs");
const RUNTIME_WORLD_IDENTITY_WIRE_RS: &str = include_str!("../world/identity_wire.rs");
const RUNTIME_WORLD_GROUNDING_RS: &str = include_str!("../world/grounding.rs");
const RUNTIME_WORLD_PROTOCOL_RS: &str = include_str!("../world/protocol.rs");
const RUNTIME_WORLD_RECEIPT_RS: &str = include_str!("../world/receipt.rs");
const RUNTIME_WORLD_RECEIPT_CODEC_RS: &str = include_str!("../world/receipt_codec.rs");
const RUNTIME_WORLD_VALUE_RS: &str = include_str!("../world/value.rs");
const RUNTIME_WORLD_VALUE_CODEC_RS: &str = include_str!("../world/value_codec.rs");

// hgraph — hypergraph substrate used by ir.rs and eval.rs at runtime.
const RUNTIME_HGRAPH_MOD_RS: &str = include_str!("../hgraph/mod.rs");
const RUNTIME_HGRAPH_GRAPH_RS: &str = include_str!("../hgraph/graph.rs");
const RUNTIME_HGRAPH_KINDS_RS: &str = include_str!("../hgraph/kinds.rs");
const RUNTIME_HGRAPH_FROM_OIR_RS: &str = include_str!("../hgraph/from_oir.rs");
const RUNTIME_HGRAPH_SCHEDULE_RS: &str = include_str!("../hgraph/schedule.rs");
const RUNTIME_HGRAPH_SOLVE_RS: &str = include_str!("../hgraph/solve.rs");

// executor: the readiness-driven graph coordinator used by eval.rs as the
// default execution engine, with its serial reference path retained in eval.rs.
const RUNTIME_EXECUTOR_MOD_RS: &str = include_str!("../executor/mod.rs");
const RUNTIME_EXECUTOR_ACTOR_RS: &str = include_str!("../executor/actor.rs");
const RUNTIME_EXECUTOR_CANCELLATION_RS: &str = include_str!("../executor/cancellation.rs");
const RUNTIME_EXECUTOR_COORDINATOR_RS: &str = include_str!("../executor/coordinator.rs");
const RUNTIME_EXECUTOR_EFFECTS_RS: &str = include_str!("../executor/effects.rs");
const RUNTIME_EXECUTOR_PARALLEL_RS: &str = include_str!("../executor/parallel.rs");
const RUNTIME_EXECUTOR_POOL_RS: &str = include_str!("../executor/pool.rs");
const RUNTIME_EXECUTOR_TASK_RS: &str = include_str!("../executor/task.rs");
const RUNTIME_EXECUTOR_TRACE_RS: &str = include_str!("../executor/trace.rs");

// project — first-class project/route/bundle model, embedded so compiled
// project binaries can materialize and run their embedded routes.
const RUNTIME_PROJECT_SOURCES: &[(&str, &str)] = &[
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

// Cargo.lock from the workspace — embedded so the temp project gets identical
// resolved dependency versions (Cargo may still download an absent crate).
// The generated root package entry is projected to a fixed non-colliding
// package name and the generated runtime's smaller dependency set before the
// lock is written.
const WORKSPACE_CARGO_LOCK: &[u8] = include_bytes!("../../Cargo.lock");
const WORKSPACE_RUST_TOOLCHAIN_TOML: &[u8] = include_bytes!("../../rust-toolchain.toml");
const GENERATED_PACKAGE_NAME: &str = "ostadix-generated-runtime";
const GENERATED_PACKAGE_VERSION: &str = "0.1.0";
const GENERATED_RUNTIME_DEPENDENCY_NAMES: &[&str] = &[
    "anyhow",
    "base64",
    "bitflags",
    "clap",
    "ed25519-dalek",
    "getrandom",
    "hex",
    "libc",
    "num-bigint",
    "num-traits",
    "semver",
    "serde",
    "serde_json",
    "sha2",
    "thiserror",
    "toml",
    "which",
];

// ─────────────────────────────────────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────────────────────────────────────

/// Compilation target: the small internal CompileTarget abstraction.
/// Each variant selects one end-to-end pipeline over the shared front end
/// (read source → parse → OIR): native codegen via Cargo, in-process OIR
/// execution, or an executable OIR dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CompileTarget {
    /// Compile to a self-contained native binary on disk (ELF/Mach-O).
    Binary,
    /// Compile to a WebAssembly (WASI) binary on disk.
    Wasm,
    /// Execute the lowered and planned OIR inside the olangc process.
    Script,
    /// Print a non-executing logical plan: OIR/ExecutionPlan/HGraph for `.O`,
    /// or ProjectExecutionPlan/HGraph for a directory or lifted project.
    Ir,
    /// Lower the parsed program to OIR, build its HGraph, solve types, and
    /// emit a Graphviz DOT digraph to stdout. Pipe to `dot -Tpng` for a PNG.
    Dot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ScheduleExplanationFormat {
    /// Preserve the historical OIR, HGraph, and human admission explanation.
    Text,
    /// Emit one versioned schedule-explanation JSON document.
    Json,
}

#[derive(ClapParser, Debug)]
#[command(
    name = "olangc",
    about = "Compile or run a .O program",
    long_about = "\
Compiles a .O source file into a native binary (--target binary, the default), \
a wasm32-wasip1 module (--target wasm), executes in-process (--target script), \
prints the lowered OIR/ExecutionPlan/HGraph or project plan/HGraph (--target ir), \
or emits the execution hypergraph as Graphviz DOT (--target dot). Binary \
outputs embed the program source, compatibility adapters, and the Ostadix-lang \
runtime. Project IR/DOT planning constructs route operations without running \
commands. In dot mode the HGraph is serialised as a digraph; pipe to \
`dot -Tpng` for a rendered image."
)]
struct Cli {
    /// A .O source/lifted bundle or project directory to compile, run, or plan
    input: PathBuf,

    /// Compilation target
    #[arg(long, value_enum, default_value_t = CompileTarget::Binary)]
    target: CompileTarget,

    /// Output binary path (default: input file stem in the current directory).
    /// Ignored when --target is "script", "ir", or "dot".
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Override or extend the bundled compatibility adapters with files from
    /// this directory. Files with names matching a bundled adapter replace it;
    /// files with new names are added. If omitted, olangc uses only its
    /// built-in adapters, so it works from any working directory.
    #[arg(long)]
    shim_dir: Option<PathBuf>,

    /// Keep the intermediate build directory after compilation (useful for
    /// debugging; relevant for binary and wasm targets)
    #[arg(long)]
    keep_build_dir: bool,

    /// Compatibility hook: mint a live backend capability at startup and bind
    /// it in O scope. Normal hosted backends already have default host authority.
    /// Format: `NAME=LANG[:fs_read,fs_write,network,process]`.
    #[arg(long = "backend-grant")]
    backend_grants: Vec<String>,

    /// Select one project route or route set for project script/IR/DOT modes.
    #[arg(long)]
    route: Option<String>,

    /// Override the selected project's route policy. Requires --route.
    #[arg(long = "routes-policy")]
    routes_policy: Option<String>,

    /// Write the unsigned Project HGraph attempt trace as JSON. This is
    /// available only for hosted project execution with --target script and
    /// requires O_PROJECT_EXECUTOR=hgraph.
    #[arg(long = "project-trace-out", value_name = "PATH")]
    project_trace_out: Option<PathBuf>,

    /// Append the governed/ambient grounding report to `--target ir` output.
    /// This is a planner inspection view and does not perform placement.
    #[arg(long)]
    grounding: bool,

    /// Append the evidence-bound admission, per-operation provenance and
    /// blockers, retained source-order reasons, and legal static ready waves.
    /// This is non-executing and is currently available for ordinary .O IR.
    #[arg(long)]
    explain_schedule: bool,

    /// Select the schedule-explanation rendering. This option is valid only
    /// with --target ir --explain-schedule; omitting it preserves text output.
    #[arg(long, value_enum, value_name = "FORMAT")]
    format: Option<ScheduleExplanationFormat>,

    /// Emit one authority-free, stable JSON identity for the exact source,
    /// lowered OIR, plan, solved graph, canonical backend-catalog projection,
    /// analyzer, and base policy. This does not inspect runtime availability,
    /// authorize execution, or replace fresh AdmittedExecution validation.
    #[arg(long)]
    execution_intent_json: bool,

    /// Explain why one canonical plan operation is statically admitted or
    /// blocked. Accepts an exact plan identity such as P3. This is a focused,
    /// non-executing ordinary-.O admission view and requires --target ir.
    #[arg(long, value_name = "PLAN_NODE", value_parser = parse_plan_node_selector)]
    why: Option<PlanNodeId>,

    /// Override the local-worker count used by the non-executing schedule
    /// realizability marker. Requires --target ir --explain-schedule.
    #[arg(long, value_name = "N")]
    workers: Option<usize>,

    /// Bind grounding output to one logical World. Requires --world-epoch.
    #[arg(long)]
    world_id: Option<String>,

    /// Bind grounding output to one exact, nonzero World epoch.
    #[arg(long)]
    world_epoch: Option<u64>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    if o_lang::backend::run_backend_from_env_args()? {
        return Ok(());
    }

    let cli = Cli::parse();
    validate_admission_inspection(&cli)?;
    let grounding_world = parse_grounding_world(&cli)?;

    let input_is_dir = cli.input.is_dir();
    let source = if input_is_dir {
        String::new()
    } else {
        fs::read_to_string(&cli.input)
            .with_context(|| format!("failed to read {}", cli.input.display()))?
    };

    // A project input is either a directory or a lifted .O file carrying an
    // embedded project bundle. Projects compile into a native binary that
    // lists and runs the same routes.
    let is_project = input_is_dir || o_lang::project::lower::has_embedded_bundle(&source);
    if is_project {
        if cli.explain_schedule || cli.why.is_some() {
            bail!(
                "--explain-schedule and --why currently admit ordinary .O HGraphs only; project HGraph admission is deferred"
            );
        }
        if cli.execution_intent_json {
            bail!(
                "--execution-intent-json currently identifies ordinary .O HGraphs only; project HGraph intent is deferred"
            );
        }
        return compile_or_run_project(&cli, input_is_dir, &source);
    }
    if cli.route.is_some() || cli.routes_policy.is_some() || cli.project_trace_out.is_some() {
        bail!(
            "--route, --routes-policy, and --project-trace-out are available only for project inputs"
        );
    }

    match cli.target {
        CompileTarget::Binary | CompileTarget::Wasm => {
            // Resolve output path: default to <input stem> in cwd.
            let mut output = match cli.output {
                Some(p) => p,
                None => {
                    let stem = cli
                        .input
                        .file_stem()
                        .with_context(|| {
                            format!("input path has no file stem: {}", cli.input.display())
                        })?
                        .to_string_lossy();
                    PathBuf::from(stem.as_ref())
                }
            };

            if cli.target == CompileTarget::Wasm {
                output.set_extension("wasm");
            }

            let shims = read_shims(cli.shim_dir.as_deref())?;

            let build_dir = create_build_dir()?;
            eprintln!("olangc: building in {}", build_dir.display());
            eprintln!("olangc: embedding {} shim script(s)", shims.len());

            let result = compile_to_binary(
                &cli.input,
                &source,
                &shims,
                &build_dir,
                &output,
                cli.target == CompileTarget::Wasm,
                &cli.backend_grants,
            );

            if !cli.keep_build_dir {
                let _ = fs::remove_dir_all(&build_dir);
            } else {
                eprintln!("olangc: keeping build directory: {}", build_dir.display());
            }

            result
        }
        CompileTarget::Script => {
            run_as_script(&source, cli.shim_dir.as_deref(), &cli.backend_grants)
        }
        CompileTarget::Ir if cli.execution_intent_json => dump_execution_intent_json(&source),
        CompileTarget::Ir if cli.why.is_some() => dump_schedule_why(
            &cli.input,
            &source,
            cli.shim_dir.as_deref(),
            cli.why
                .expect("IR why branch requires a validated selector"),
        ),
        CompileTarget::Ir if cli.explain_schedule => dump_ir_with_admission(
            &source,
            cli.shim_dir.as_deref(),
            cli.grounding,
            grounding_world,
            cli.workers,
            cli.format.unwrap_or(ScheduleExplanationFormat::Text),
        ),
        CompileTarget::Ir if cli.grounding => dump_ir_with_grounding(&source, grounding_world),
        CompileTarget::Ir => dump_ir(&source),
        CompileTarget::Dot => dump_dot(&source),
    }
}

fn validate_admission_inspection(cli: &Cli) -> Result<()> {
    if cli.explain_schedule && cli.target != CompileTarget::Ir {
        bail!("--explain-schedule is available only with --target ir");
    }
    if cli.workers == Some(0) {
        bail!("--workers must be at least 1");
    }
    if cli.workers.is_some() && !cli.explain_schedule {
        bail!("--workers requires --explain-schedule --target ir");
    }
    if cli.format.is_some() && !cli.explain_schedule {
        bail!("--format requires --explain-schedule --target ir");
    }
    if cli.format == Some(ScheduleExplanationFormat::Json)
        && (cli.grounding || cli.world_id.is_some() || cli.world_epoch.is_some())
    {
        bail!(
            "--format json is a standalone schedule view and cannot be combined with grounding or World inspection options"
        );
    }
    if cli.why.is_some() && cli.target != CompileTarget::Ir {
        bail!("--why is available only with --target ir");
    }
    if cli.why.is_some() && cli.explain_schedule {
        bail!("--why and --explain-schedule are distinct inspection views and cannot be combined");
    }
    if cli.execution_intent_json && cli.target != CompileTarget::Ir {
        bail!("--execution-intent-json is available only with --target ir");
    }
    if cli.execution_intent_json
        && (cli.explain_schedule
            || cli.why.is_some()
            || cli.grounding
            || cli.workers.is_some()
            || cli.world_id.is_some()
            || cli.world_epoch.is_some())
    {
        bail!(
            "--execution-intent-json is a standalone JSON inspection view and cannot be combined with schedule, why, grounding, worker, or World inspection options"
        );
    }
    if cli.why.is_some() && (cli.grounding || cli.world_id.is_some() || cli.world_epoch.is_some()) {
        bail!("--why cannot be combined with --grounding, --world-id, or --world-epoch");
    }
    Ok(())
}

fn parse_plan_node_selector(value: &str) -> std::result::Result<PlanNodeId, String> {
    let digits = value
        .strip_prefix('P')
        .ok_or_else(|| "expected a canonical plan node such as P3".to_string())?;
    if digits.is_empty()
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return Err("expected a canonical plan node such as P3".to_string());
    }
    digits
        .parse::<usize>()
        .map(PlanNodeId)
        .map_err(|_| format!("plan node `{value}` is outside this platform's supported range"))
}

fn parse_grounding_world(cli: &Cli) -> Result<Option<WorldIdentity>> {
    if cli.grounding && cli.target != CompileTarget::Ir {
        bail!("--grounding is available only with --target ir");
    }
    if !cli.grounding && (cli.world_id.is_some() || cli.world_epoch.is_some()) {
        bail!("--world-id and --world-epoch require --grounding --target ir");
    }
    match (&cli.world_id, cli.world_epoch) {
        (None, None) => Ok(None),
        (Some(world), Some(epoch)) => Ok(Some(WorldIdentity::new(
            WorldId::new(world.clone())?,
            WorldEpoch::new(epoch)?,
        ))),
        (Some(_), None) => bail!("--world-id requires --world-epoch"),
        (None, Some(_)) => bail!("--world-epoch requires --world-id"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Project compilation
// ─────────────────────────────────────────────────────────────────────────────

/// Build the `ProjectBundle` for a project input (a directory or a lifted
/// `.O` file with an embedded bundle).
fn load_project_bundle(
    input: &Path,
    input_is_dir: bool,
    source: &str,
    exclusions: &[PathBuf],
) -> Result<o_lang::project::ProjectBundle> {
    if input_is_dir {
        let name = o_lang::project::name_from_path(input);
        o_lang::project::assemble_excluding(input, &name, &[], exclusions)
    } else {
        o_lang::project::lower::extract_bundle_from_o(source)
    }
}

/// Dispatch a project input to the right pipeline based on the compile target.
fn compile_or_run_project(cli: &Cli, input_is_dir: bool, source: &str) -> Result<()> {
    use o_lang::project::RoutePolicy;

    if cli.project_trace_out.is_some() && cli.target != CompileTarget::Script {
        bail!(
            "--project-trace-out requires hosted project execution with --target script; compiled project binaries accept this option at runtime"
        );
    }
    let exclusions = cli
        .project_trace_out
        .iter()
        .cloned()
        .collect::<Vec<PathBuf>>();
    let bundle = load_project_bundle(&cli.input, input_is_dir, source, &exclusions)?;
    let policy_override = cli
        .routes_policy
        .as_deref()
        .map(RoutePolicy::parse_checked)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    if policy_override.is_some() && cli.route.is_none() {
        bail!("--routes-policy requires --route to name a project route or route set");
    }
    match cli.target {
        CompileTarget::Binary | CompileTarget::Wasm => {
            if cli.route.is_some() || policy_override.is_some() {
                bail!(
                    "--route and --routes-policy select project script/IR/DOT plans; compiled project binaries accept them at runtime"
                );
            }

            let mut output = match cli.output.clone() {
                Some(p) => p,
                None => {
                    let stem = if input_is_dir {
                        o_lang::project::name_from_path(&cli.input)
                    } else {
                        cli.input
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| "project".to_string())
                    };
                    PathBuf::from(stem)
                }
            };
            if cli.target == CompileTarget::Wasm {
                output.set_extension("wasm");
            }

            let shims = read_shims(cli.shim_dir.as_deref())?;
            let build_dir = create_build_dir()?;
            eprintln!("olangc: building project in {}", build_dir.display());
            eprintln!(
                "olangc: project '{}' — {} file(s), {} route(s)",
                bundle.name,
                bundle.files.len(),
                bundle.routes.len()
            );

            let result = compile_project_to_binary(
                &bundle,
                &shims,
                &build_dir,
                &output,
                cli.target == CompileTarget::Wasm,
            );

            if !cli.keep_build_dir {
                let _ = fs::remove_dir_all(&build_dir);
            } else {
                eprintln!("olangc: keeping build directory: {}", build_dir.display());
            }
            result
        }
        CompileTarget::Script => run_project_script(
            &bundle,
            cli.route.as_deref(),
            policy_override,
            cli.project_trace_out.as_deref(),
        ),
        CompileTarget::Ir => {
            if cli.grounding {
                bail!(
                    "--grounding for project inputs is deferred to the PR9 project-grounding view"
                );
            }
            let project = o_lang::project::build_project_hgraph(
                &bundle,
                cli.route.as_deref(),
                policy_override,
            )
            .map_err(anyhow::Error::msg)
            .context("failed to build logical project HGraph")?;
            let logical = project
                .logical_v1()
                .context("failed to normalize LogicalHGraphV1")?;
            let logical_digest = logical
                .digest()
                .context("failed to digest LogicalHGraphV1")?;
            let deployment = o_lang::project::DeploymentPlanV1::hosted(&logical)
                .context("failed to construct hosted DeploymentPlanV1")?;
            let deployment_digest = deployment
                .digest()
                .context("failed to digest hosted DeploymentPlanV1")?;
            println!("; LogicalHGraphV1");
            println!(
                "logical schema={} sha256={}",
                logical.schema_version,
                logical_digest.as_sha256()
            );
            println!("; DeploymentPlanV1");
            println!(
                "deployment schema={} sha256={}",
                deployment.schema_version,
                deployment_digest.as_sha256()
            );
            print!("{}{}", project.to_text(), deployment.to_text());
            Ok(())
        }
        CompileTarget::Dot => {
            let project = o_lang::project::build_project_hgraph(
                &bundle,
                cli.route.as_deref(),
                policy_override,
            )
            .map_err(anyhow::Error::msg)
            .context("failed to build logical project HGraph")?;
            print!("{}", hgraph_to_dot(&project.graph));
            Ok(())
        }
    }
}

/// Run a project's default route in-process (script mode).
fn run_project_script(
    bundle: &o_lang::project::ProjectBundle,
    route: Option<&str>,
    policy: Option<o_lang::project::RoutePolicy>,
    trace_out: Option<&Path>,
) -> Result<()> {
    use o_lang::project::runtime::RunOptions;
    eprintln!("olangc: project script mode — running resolved route selection");
    let results =
        execute_project_selection(bundle, route, policy, &RunOptions::default(), trace_out)?;
    for result in &results {
        print!("{}", result.summary());
    }
    if !results.iter().any(|r| r.succeeded()) {
        bail!("no route succeeded");
    }
    Ok(())
}

/// Execute through the configured project runtime and, when requested, retain
/// the unsigned HGraph diagnostic trace on both successful and failed attempts.
fn execute_project_selection(
    bundle: &o_lang::project::ProjectBundle,
    route: Option<&str>,
    policy: Option<o_lang::project::RoutePolicy>,
    opts: &o_lang::project::runtime::RunOptions,
    trace_out: Option<&Path>,
) -> Result<Vec<o_lang::project::OExecutionResult>> {
    use o_lang::project::executor::{
        execute_selection_with_configured_executor, write_project_attempt_trace,
        ProjectExecutionError, PROJECT_EXECUTOR_ENV,
    };

    if trace_out.is_some()
        && std::env::var_os(PROJECT_EXECUTOR_ENV).as_deref() != Some(std::ffi::OsStr::new("hgraph"))
    {
        bail!(
            "--project-trace-out requires {PROJECT_EXECUTOR_ENV}=hgraph; the legacy project runtime does not produce a Project HGraph attempt trace"
        );
    }

    let execution = match execute_selection_with_configured_executor(bundle, route, policy, opts) {
        Ok(execution) => execution,
        Err(error) => {
            if let (Some(path), Some(project_error)) =
                (trace_out, error.downcast_ref::<ProjectExecutionError>())
            {
                if let Err(trace_error) = write_project_attempt_trace(path, &project_error.trace) {
                    return Err(error.context(format!(
                        "additionally failed to retain the Project HGraph attempt trace: {trace_error:#}"
                    )));
                }
            }
            return Err(error);
        }
    };

    if let Some(path) = trace_out {
        let trace = execution
            .trace
            .as_ref()
            .context("HGraph project execution returned no Project HGraph attempt trace")?;
        write_project_attempt_trace(path, trace)?;
    }

    Ok(execution.results)
}

/// Generate a hosted Cargo project that embeds the serialized bundle and, at
/// runtime, lists/materializes/runs the same routes via the project runtime.
fn compile_project_to_binary(
    bundle: &o_lang::project::ProjectBundle,
    shims: &[(String, Vec<u8>)],
    build_dir: &Path,
    output: &Path,
    is_wasm: bool,
) -> Result<()> {
    let bin_name = derive_bin_name(output);
    write_project_cargo_project(bundle, shims, build_dir, &bin_name)?;

    let mut cargo_args = vec!["build", "--release", "--locked"];
    if is_wasm {
        cargo_args.push("--target");
        cargo_args.push("wasm32-wasip1");
        eprintln!("olangc: running cargo build --release --locked --target wasm32-wasip1 ...");
    } else {
        eprintln!("olangc: running cargo build --release --locked ...");
    }

    let status = Command::new("cargo")
        .args(&cargo_args)
        .current_dir(build_dir)
        .status()
        .context("failed to spawn cargo — is Rust/Cargo installed?")?;
    if !status.success() {
        bail!("cargo build --release failed (see output above)");
    }

    let built = built_binary_path(build_dir, &bin_name, is_wasm);
    let dest = canonicalize_output(output)?;
    fs::copy(&built, &dest)
        .with_context(|| format!("failed to copy {} → {}", built.display(), dest.display()))?;

    #[cfg(unix)]
    if !is_wasm {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
    }

    eprintln!("olangc: compiled project → {}", dest.display());
    Ok(())
}

/// Materialize the complete Cargo source tree used for a compiled project.
///
/// Keeping this separate from the Cargo invocation lets the closure regression
/// compile exactly the same generated crate that production builds.
fn write_project_cargo_project(
    bundle: &o_lang::project::ProjectBundle,
    shims: &[(String, Vec<u8>)],
    build_dir: &Path,
    bin_name: &str,
) -> Result<()> {
    let src_dir = build_dir.join("src");
    let shim_dir = src_dir.join("shims");
    fs::create_dir_all(&shim_dir)?;

    write_runtime_sources(&src_dir)?;
    write_project_sources(&src_dir)?;

    // Embed the serialized bundle as bytes.
    let bundle_bytes = o_lang::project::bundle::serialize(bundle)?;
    fs::write(src_dir.join("project_bundle.json"), &bundle_bytes)?;

    // Shim scripts.
    let mut shim_include_lines = Vec::new();
    for (name, content) in shims {
        fs::write(shim_dir.join(name), content)?;
        shim_include_lines.push(format!(
            "    ({name:?}, include_bytes!({path:?})),",
            name = name,
            path = format!("shims/{name}"),
        ));
    }

    let lib_rs = generate_lib_rs(true);
    fs::write(src_dir.join("lib.rs"), &lib_rs)?;
    let main_rs = generate_project_main_rs(bin_name, &shim_include_lines);
    fs::write(src_dir.join("main.rs"), &main_rs)?;

    write_generated_cargo_contract(build_dir, bin_name, true)?;
    Ok(())
}

/// Write the embedded `project` module tree into the generated `src/`.
fn write_project_sources(src_dir: &Path) -> Result<()> {
    let project_dir = src_dir.join("project");
    for &(relative_path, source) in RUNTIME_PROJECT_SOURCES {
        let destination = project_dir.join(relative_path);
        let parent = destination
            .parent()
            .with_context(|| format!("generated project source has no parent: {relative_path}"))?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create generated project source directory {}",
                parent.display()
            )
        })?;
        fs::write(&destination, source).with_context(|| {
            format!(
                "failed to write generated project source {}",
                destination.display()
            )
        })?;
    }
    Ok(())
}

/// Generate the `main.rs` for a compiled project binary. It embeds the bundle
/// and supports `--list-routes`, `--route <ID>`, `--routes-policy <POLICY>`,
/// `--project-trace-out <PATH>`, and default-route execution.
fn generated_lib_name(bin_name: &str) -> String {
    // Cargo exposes both the package library and every direct dependency to
    // the generated binary. A user output such as `serde` would otherwise
    // create two crates with the same extern name. Keep the target dynamic,
    // but reserve an Ostadix namespace distinct from this runtime's direct
    // dependency names.
    format!("ostadix_generated_{}", bin_name.replace('-', "_"))
}

fn generate_project_main_rs(bin_name: &str, shim_include_lines: &[String]) -> String {
    let lib_name = generated_lib_name(bin_name);
    let shim_entries = if shim_include_lines.is_empty() {
        "    // no shims bundled".to_string()
    } else {
        shim_include_lines.join("\n")
    };

    format!(
        r###"// AUTO-GENERATED by olangc. DO NOT EDIT.

use ::{lib_name}::project::RoutePolicy;
use ::{lib_name}::project::executor::{{
    execute_selection_with_configured_executor, write_project_attempt_trace,
    ProjectExecutionError, PROJECT_EXECUTOR_ENV,
}};
use ::{lib_name}::project::runtime::RunOptions;

/// The serialized project bundle, embedded at compile time.
const PROJECT_BUNDLE: &[u8] = include_bytes!("project_bundle.json");

#[cfg(not(target_family = "wasm"))]
const EMBEDDED_SHIMS: &[(&str, &[u8])] = &[
{shim_entries}
];

fn main() -> anyhow::Result<()> {{
    if ::{lib_name}::backend::run_backend_from_env_args()? {{
        return Ok(());
    }}

    #[cfg(not(target_family = "wasm"))]
    let _ = EMBEDDED_SHIMS;

    let bundle = ::{lib_name}::project::bundle::deserialize(PROJECT_BUNDLE)?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut list_routes = false;
    let mut route: Option<String> = None;
    let mut policy: Option<String> = None;
    let mut trace_out: Option<std::path::PathBuf> = None;
    let mut i = 0;
    while i < args.len() {{
        match args[i].as_str() {{
            "--list-routes" => list_routes = true,
            "--route" => {{
                i += 1;
                route = Some(
                    args.get(i)
                        .filter(|value| !value.starts_with("--"))
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--route requires a value"))?,
                );
            }}
            "--routes-policy" => {{
                i += 1;
                policy = Some(
                    args.get(i)
                        .filter(|value| !value.starts_with("--"))
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--routes-policy requires a value"))?,
                );
            }}
            "--project-trace-out" => {{
                i += 1;
                trace_out = Some(std::path::PathBuf::from(
                    args.get(i)
                        .filter(|value| !value.starts_with("--"))
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--project-trace-out requires a path"))?,
                ));
            }}
            other => return Err(anyhow::anyhow!("unknown argument: {{other}}")),
        }}
        i += 1;
    }}

    if list_routes {{
        if trace_out.is_some() {{
            return Err(anyhow::anyhow!(
                "--project-trace-out requires project execution and cannot be combined with --list-routes"
            ));
        }}
        print!("{{}}", bundle.route_table());
        return Ok(());
    }}

    if trace_out.is_some()
        && std::env::var_os(PROJECT_EXECUTOR_ENV).as_deref()
            != Some(std::ffi::OsStr::new("hgraph"))
    {{
        return Err(anyhow::anyhow!(
            "--project-trace-out requires {{PROJECT_EXECUTOR_ENV}}=hgraph; the legacy project runtime does not produce a Project HGraph attempt trace"
        ));
    }}

    let policy = policy
        .as_deref()
        .map(RoutePolicy::parse_checked)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let opts = RunOptions::default();
    let execution = match execute_selection_with_configured_executor(
        &bundle,
        route.as_deref(),
        policy,
        &opts,
    ) {{
        Ok(execution) => execution,
        Err(error) => {{
            if let (Some(path), Some(project_error)) = (
                trace_out.as_deref(),
                error.downcast_ref::<ProjectExecutionError>(),
            ) {{
                if let Err(trace_error) = write_project_attempt_trace(path, &project_error.trace) {{
                    return Err(error.context(format!(
                        "additionally failed to retain the Project HGraph attempt trace: {{trace_error:#}}"
                    )));
                }}
            }}
            return Err(error);
        }}
    }};
    if let Some(path) = trace_out.as_deref() {{
        let trace = execution.trace.as_ref().ok_or_else(|| anyhow::anyhow!(
            "HGraph project execution returned no Project HGraph attempt trace"
        ))?;
        write_project_attempt_trace(path, trace)?;
    }}
    let results = execution.results;
    for result in &results {{
        print!("{{}}", result.summary());
    }}
    if !results.iter().any(|r| r.succeeded()) {{
        std::process::exit(1);
    }}
    Ok(())
}}
"###,
        lib_name = lib_name,
        shim_entries = shim_entries,
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Target A — compile to a native binary on disk
// ─────────────────────────────────────────────────────────────────────────────

fn compile_to_binary(
    input_path: &Path,
    source: &str,
    shims: &[(String, Vec<u8>)],
    build_dir: &Path,
    output: &Path,
    is_wasm: bool,
    backend_grants: &[String],
) -> Result<()> {
    let bin_name = derive_bin_name(output);
    let src_dir = build_dir.join("src");
    let shim_dir = src_dir.join("shims");
    fs::create_dir_all(&shim_dir)?;

    // ── Runtime source files ─────────────────────────────────────────────────
    write_runtime_sources(&src_dir)?;

    // ── Program source ───────────────────────────────────────────────────────
    // Always stored as "program.O" so the generated main.rs can reference it
    // with a known fixed name regardless of the original filename.
    let program_filename = sanitize_program_filename(input_path);
    fs::write(src_dir.join(&program_filename), source)?;

    // ── Shim scripts ─────────────────────────────────────────────────────────
    let mut shim_include_lines = Vec::new();
    for (name, content) in shims {
        fs::write(shim_dir.join(name), content)?;
        // include_bytes! path is relative to the src/ directory.
        shim_include_lines.push(format!(
            "    ({name:?}, include_bytes!({path:?})),",
            name = name,
            path = format!("shims/{name}"),
        ));
    }

    // ── Generated lib.rs and main.rs ────────────────────────────────────────
    let lib_rs = generate_lib_rs(false);
    fs::write(src_dir.join("lib.rs"), &lib_rs)?;
    let main_rs = generate_main_rs(
        &bin_name,
        &program_filename,
        &shim_include_lines,
        backend_grants,
    );
    fs::write(src_dir.join("main.rs"), &main_rs)?;

    // ── Cargo build contract — manifest, exact lock, pinned toolchain ─────────
    write_generated_cargo_contract(build_dir, &bin_name, false)?;

    // ── Build ────────────────────────────────────────────────────────────────
    let mut cargo_args = vec!["build", "--release", "--locked"];
    if is_wasm {
        cargo_args.push("--target");
        cargo_args.push("wasm32-wasip1");
        eprintln!("olangc: running cargo build --release --locked --target wasm32-wasip1 ...");
    } else {
        eprintln!("olangc: running cargo build --release --locked ...");
    }

    let status = Command::new("cargo")
        .args(&cargo_args)
        .current_dir(build_dir)
        .status()
        .context("failed to spawn cargo — is Rust/Cargo installed?")?;

    if !status.success() {
        bail!("cargo build --release failed (see output above)");
    }

    // ── Copy binary to output ────────────────────────────────────────────────
    let built = built_binary_path(build_dir, &bin_name, is_wasm);
    let dest = canonicalize_output(output)?;

    fs::copy(&built, &dest)
        .with_context(|| format!("failed to copy {} → {}", built.display(), dest.display()))?;

    // Make the output binary executable on Unix.
    #[cfg(unix)]
    if !is_wasm {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
    }

    eprintln!("olangc: compiled → {}", dest.display());
    Ok(())
}

fn write_runtime_sources(src_dir: &Path) -> Result<()> {
    fs::create_dir_all(src_dir)?;
    fs::write(src_dir.join("value.rs"), RUNTIME_VALUE_RS)?;
    fs::write(src_dir.join("capability.rs"), RUNTIME_CAPABILITY_RS)?;
    fs::write(src_dir.join("environment.rs"), RUNTIME_ENVIRONMENT_RS)?;
    fs::write(src_dir.join("parser.rs"), RUNTIME_PARSER_RS)?;
    fs::write(src_dir.join("ir.rs"), RUNTIME_IR_RS)?;
    fs::write(
        src_dir.join("backend_catalog.rs"),
        RUNTIME_BACKEND_CATALOG_MODULE_RS,
    )?;
    fs::write(
        src_dir.join("backend_catalog.inc.rs"),
        RUNTIME_BACKEND_CATALOG_DATA_RS,
    )?;
    fs::write(
        src_dir.join("execution_contract.rs"),
        RUNTIME_EXECUTION_CONTRACT_RS,
    )?;
    fs::write(src_dir.join("eval_core.rs"), RUNTIME_EVAL_CORE_RS)?;
    fs::write(src_dir.join("eval.rs"), RUNTIME_EVAL_RS)?;
    fs::write(src_dir.join("process.rs"), RUNTIME_PROCESS_RS)?;
    fs::write(src_dir.join("backend.rs"), RUNTIME_BACKEND_RS)?;
    fs::write(
        src_dir.join("backend_morphism.rs"),
        RUNTIME_BACKEND_MORPHISM_RS,
    )?;
    fs::write(src_dir.join("backend_state.rs"), RUNTIME_BACKEND_STATE_RS)?;
    fs::write(src_dir.join("nix_ops.rs"), RUNTIME_NIX_OPS_RS)?;
    fs::write(src_dir.join("nixos_ops.rs"), RUNTIME_NIXOS_OPS_RS)?;
    fs::write(src_dir.join("scheduler.rs"), RUNTIME_SCHEDULER_RS)?;
    fs::write(src_dir.join("canonical_cbor.rs"), RUNTIME_CANONICAL_CBOR_RS)?;
    fs::write(src_dir.join("dispatch_model.rs"), RUNTIME_DISPATCH_MODEL_RS)?;
    fs::write(src_dir.join("syntax_dialect.rs"), RUNTIME_SYNTAX_DIALECT_RS)?;
    fs::write(src_dir.join("wire.rs"), RUNTIME_WIRE_RS)?;
    fs::write(src_dir.join("effects.rs"), RUNTIME_EFFECTS_RS)?;
    fs::write(src_dir.join("runtime_exec.rs"), RUNTIME_RUNTIME_EXEC_RS)?;

    // ── placement/catalog — shared semantic identity and capabilities ──────
    let placement_dir = src_dir.join("placement");
    for &(relative_path, source) in RUNTIME_PLACEMENT_SOURCES {
        let destination = placement_dir.join(relative_path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(destination, source)?;
    }
    let registry_bundle_dir = src_dir.join("registry").join("bundle");
    fs::create_dir_all(&registry_bundle_dir)?;
    fs::write(
        src_dir.join("registry/mod.rs"),
        "pub mod bundle;\nmod placement_compat;\n",
    )?;
    fs::write(
        registry_bundle_dir.join("mod.rs"),
        RUNTIME_REGISTRY_BUNDLE_RS,
    )?;
    fs::write(
        src_dir.join("registry/placement_compat.rs"),
        RUNTIME_REGISTRY_PLACEMENT_COMPAT_RS,
    )?;

    // ── evidence — evidence-bound execution admission ──────────────────────
    let evidence_dir = src_dir.join("evidence");
    fs::create_dir_all(&evidence_dir)?;
    fs::write(evidence_dir.join("mod.rs"), RUNTIME_EVIDENCE_MOD_RS)?;
    fs::write(evidence_dir.join("fact.rs"), RUNTIME_EVIDENCE_FACT_RS)?;
    fs::write(evidence_dir.join("analyze.rs"), RUNTIME_EVIDENCE_ANALYZE_RS)?;
    fs::write(evidence_dir.join("admit.rs"), RUNTIME_EVIDENCE_ADMIT_RS)?;
    fs::write(evidence_dir.join("intent.rs"), RUNTIME_EVIDENCE_INTENT_RS)?;
    fs::write(evidence_dir.join("profile.rs"), RUNTIME_EVIDENCE_PROFILE_RS)?;

    // ── world — governed identity/effect vocabulary ────────────────────────
    let world_dir = src_dir.join("world");
    fs::create_dir_all(&world_dir)?;
    fs::write(world_dir.join("mod.rs"), RUNTIME_WORLD_MOD_RS)?;
    fs::write(world_dir.join("codec.rs"), RUNTIME_WORLD_CODEC_RS)?;
    fs::write(world_dir.join("identity.rs"), RUNTIME_WORLD_IDENTITY_RS)?;
    fs::write(
        world_dir.join("identity_wire.rs"),
        RUNTIME_WORLD_IDENTITY_WIRE_RS,
    )?;
    fs::write(world_dir.join("grounding.rs"), RUNTIME_WORLD_GROUNDING_RS)?;
    fs::write(world_dir.join("protocol.rs"), RUNTIME_WORLD_PROTOCOL_RS)?;
    fs::write(world_dir.join("receipt.rs"), RUNTIME_WORLD_RECEIPT_RS)?;
    fs::write(
        world_dir.join("receipt_codec.rs"),
        RUNTIME_WORLD_RECEIPT_CODEC_RS,
    )?;
    fs::write(world_dir.join("value.rs"), RUNTIME_WORLD_VALUE_RS)?;
    fs::write(
        world_dir.join("value_codec.rs"),
        RUNTIME_WORLD_VALUE_CODEC_RS,
    )?;

    // ── hgraph — hypergraph substrate (used by ir.rs and eval.rs) ───────────
    let hgraph_dir = src_dir.join("hgraph");
    fs::create_dir_all(&hgraph_dir)?;
    fs::write(hgraph_dir.join("mod.rs"), RUNTIME_HGRAPH_MOD_RS)?;
    fs::write(hgraph_dir.join("graph.rs"), RUNTIME_HGRAPH_GRAPH_RS)?;
    fs::write(hgraph_dir.join("kinds.rs"), RUNTIME_HGRAPH_KINDS_RS)?;
    fs::write(hgraph_dir.join("from_oir.rs"), RUNTIME_HGRAPH_FROM_OIR_RS)?;
    fs::write(hgraph_dir.join("schedule.rs"), RUNTIME_HGRAPH_SCHEDULE_RS)?;
    fs::write(hgraph_dir.join("solve.rs"), RUNTIME_HGRAPH_SOLVE_RS)?;

    // ── executor: state-complete graph coordinator ──────────────────────────
    let executor_dir = src_dir.join("executor");
    fs::create_dir_all(&executor_dir)?;
    fs::write(executor_dir.join("mod.rs"), RUNTIME_EXECUTOR_MOD_RS)?;
    fs::write(executor_dir.join("actor.rs"), RUNTIME_EXECUTOR_ACTOR_RS)?;
    fs::write(
        executor_dir.join("cancellation.rs"),
        RUNTIME_EXECUTOR_CANCELLATION_RS,
    )?;
    fs::write(
        executor_dir.join("coordinator.rs"),
        RUNTIME_EXECUTOR_COORDINATOR_RS,
    )?;
    fs::write(executor_dir.join("effects.rs"), RUNTIME_EXECUTOR_EFFECTS_RS)?;
    fs::write(
        executor_dir.join("parallel.rs"),
        RUNTIME_EXECUTOR_PARALLEL_RS,
    )?;
    fs::write(executor_dir.join("pool.rs"), RUNTIME_EXECUTOR_POOL_RS)?;
    fs::write(executor_dir.join("task.rs"), RUNTIME_EXECUTOR_TASK_RS)?;
    fs::write(executor_dir.join("trace.rs"), RUNTIME_EXECUTOR_TRACE_RS)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Target B — execute in-process (script mode)
//
// The Ostadix-lang runtime (parser, evaluator, value system) is already compiled
// into the olangc binary.  Script mode invokes that code directly: the
// machine code sitting in the .text section of the running olangc process
// is the "executable memory buffer" — loaded and mapped by the OS at
// program start.  We cast a function pointer to the evaluator entry point
// and call it, which is semantically identical to emitting code into an
// mmap'd RWX buffer and jumping to it, but without the complexity of
// relocations, dynamic linking, or ELF/Mach-O parsing.
// ─────────────────────────────────────────────────────────────────────────────

fn run_as_script(
    source: &str,
    override_shim_dir: Option<&Path>,
    backend_grants: &[String],
) -> Result<()> {
    // ── Extract shims to a temp directory ────────────────────────────────────
    // Script mode extracts compatibility adapters for backends that still need
    // them, while Rust-native backends run through the current executable.
    let shims = read_shims(override_shim_dir)?;
    let shim_dir = std::env::temp_dir().join(format!("o_shims_{}", std::process::id()));
    fs::create_dir_all(&shim_dir)?;

    // RAII guard: clean up the temp shim directory when we leave scope.
    struct ShimGuard(PathBuf);
    impl Drop for ShimGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _guard = ShimGuard(shim_dir.clone());

    for (name, content) in &shims {
        let dest = shim_dir.join(name);
        fs::write(&dest, content).with_context(|| format!("failed to extract shim {name}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
        }
    }

    eprintln!("olangc: script mode — executing in-process");
    eprintln!("olangc: using {} shim script(s)", shims.len());

    // ── Strip shebang ────────────────────────────────────────────────────────
    let src = strip_shebang(source);

    // ── Registered backends (same set as the O interpreter) ──────────────────
    let registered_backends = registered_backends();

    // ── Parse ────────────────────────────────────────────────────────────────
    let mut parser = Parser::new(&src, &registered_backends);
    let nodes = parser.parse().context("failed to parse .O source")?;

    // ── Evaluate via the already-compiled runtime (the "JIT" path) ───────────
    // The evaluator entry point is a regular Rust function whose machine code
    // lives in the executable pages of this process.  Calling it is equivalent
    // to casting a function pointer to mmap'd code and invoking it.
    let eval_fn = |shim_path: &Path,
                   backends: HashSet<String>,
                   nodes: Vec<o_lang::parser::ONode>,
                   grants: &[String]|
     -> Result<OValue> {
        let mut evaluator =
            Evaluator::new(shim_path.to_path_buf()).with_registered_backends(backends);
        let mut scope = std::collections::HashMap::new();
        for grant in grants {
            evaluator.install_backend_grant(grant, &mut scope)?;
        }
        evaluator
            .eval_document_with_scope(nodes, &mut scope)
            .context("failed to evaluate program")
    };

    let result = eval_fn(&shim_dir, registered_backends, nodes, backend_grants)?;

    // ── Print result ─────────────────────────────────────────────────────────
    match result {
        OValue::Html { v } => print!("{v}"),
        OValue::Text { v } => print!("{}", v.utf8),
        other => println!("{other}"),
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Target C — dump the OIR intermediate representation
//
// Parses the program with the same front end as the other targets, lowers
// the ONode forest to OIR (see src/ir.rs), then prints the lowered program,
// ExecutionPlan, and directed executable HGraph.
// Purely an inspection/debugging surface: nothing is executed and no output
// file is produced.
// ─────────────────────────────────────────────────────────────────────────────

fn dump_ir(source: &str) -> Result<()> {
    let (program, _plan, graph) = inspect_ir(source)?;
    print!("{}\n{}", program.to_text(), graph.to_execution_text());
    Ok(())
}

/// Emit a process-stable identity of the analyzed computation without
/// inspecting adapters or compiling execution authority. The O runtime may
/// compare this identity before dispatch, but it must still create and verify
/// a fresh live `AdmittedExecution` in that execution process.
fn dump_execution_intent_json(source: &str) -> Result<()> {
    let (program, plan, graph) = inspect_ir(source)?;
    let graph = solve_ir_admission_graph(graph)?;
    let intent =
        ExecutionIntentV1::compile(source.as_bytes(), &program, &plan, &graph, Policy::Eager)?;
    println!(
        "{}",
        serde_json::to_string(&intent).context("failed to serialize execution intent")?
    );
    Ok(())
}

fn dump_ir_with_grounding(source: &str, world: Option<WorldIdentity>) -> Result<()> {
    let (program, plan, graph) = inspect_ir(source)?;
    let grounding = GroundingReport::analyze(&plan, &graph, world)
        .context("failed to validate grounding plan/HGraph")?;
    print!(
        "{}\n{}\n{}",
        program.to_text(),
        graph.to_execution_text(),
        grounding.to_text()
    );
    Ok(())
}

/// Compile and explain the exact pre-execution admission without dispatching
/// any operation. Grounding is computed before evidence inputs are attached,
/// because it validates the analyzed graph rather than the admitted graph.
fn dump_ir_with_admission(
    source: &str,
    shim_dir: Option<&Path>,
    include_grounding: bool,
    world: Option<WorldIdentity>,
    worker_override: Option<usize>,
    format: ScheduleExplanationFormat,
) -> Result<()> {
    let (program, plan, graph) = inspect_ir(source)?;
    let graph = solve_ir_admission_graph(graph)?;
    let grounding = include_grounding
        .then(|| GroundingReport::analyze(&plan, &graph, world))
        .transpose()
        .context("failed to validate grounding plan/HGraph")?;
    let admitted = admit_ir_for_inspection(&program, &plan, graph, shim_dir)?;

    match format {
        ScheduleExplanationFormat::Text => {
            print!(
                "{}\n{}\n{}",
                program.to_text(),
                admitted.graph().to_execution_text(),
                admitted
                    .admission()
                    .to_explanation_text_with_worker_override(worker_override)
            );
            if let Some(grounding) = grounding {
                print!("\n{}", grounding.to_text());
            }
        }
        ScheduleExplanationFormat::Json => println!(
            "{}",
            admitted
                .admission()
                .to_explanation_json_with_worker_override(worker_override)
                .context("failed to serialize schedule explanation")?
        ),
    }
    Ok(())
}

/// Explain one canonical plan operation from the exact admitted HGraph. This
/// path performs parsing, lowering, solving, evidence analysis, and admission,
/// but deliberately never constructs a coordinator or dispatches an operation.
fn dump_schedule_why(
    input: &Path,
    source: &str,
    shim_dir: Option<&Path>,
    target: PlanNodeId,
) -> Result<()> {
    let (program, plan, graph, origins) = inspect_ir_with_origins(source)?;
    if origins.len() != plan.nodes.len() {
        bail!(
            "source-origin sidecar has {} entries but the canonical ExecutionPlan has {} nodes",
            origins.len(),
            plan.nodes.len()
        );
    }
    let graph = solve_ir_admission_graph(graph)?;
    let admitted = admit_ir_for_inspection(&program, &plan, graph, shim_dir)?;
    let why = admitted.schedule_why(target)?;

    print!("{}", why.to_text());
    print_schedule_why_origins(input, source, &why, &origins)?;
    let footprint =
        o_lang::placement::requirement_footprint_for_program_node(&program, &plan, target)
            .context("failed to derive Hosted Placement V6 requirement footprint")?;
    println!(
        "\n; Hosted Placement V6 requirement footprint (descriptive; not authority)\n{}",
        serde_json::to_string_pretty(&footprint)
            .context("failed to serialize placement requirement footprint")?
    );
    Ok(())
}

fn solve_ir_admission_graph(mut graph: o_lang::hgraph::HGraph) -> Result<o_lang::hgraph::HGraph> {
    o_lang::hgraph::solve::solve_types(&mut graph)
        .context("failed to solve HGraph type and fidelity constraints for admission")?;
    Ok(graph)
}

fn admit_ir_for_inspection<'a>(
    program: &'a OIrProgram,
    plan: &'a o_lang::ir::ExecutionPlan,
    graph: o_lang::hgraph::HGraph,
    shim_dir: Option<&Path>,
) -> Result<o_lang::evidence::AdmittedExecution<'a>> {
    let adapters = read_shims(shim_dir)?;
    let runtime = runtime_binding_from_adapter_bytes(
        plan,
        &adapters,
        // Both whole-admission and focused-why renderers are lenses over this
        // same inspection admission. Selecting a view must not alter its digest.
        &[("inspection-surface", "olangc-ir-explain")],
    );
    let evidence = analyze_execution(program, plan, &graph, runtime.clone())
        .context("failed to establish pre-execution evidence")?;
    admit_execution(program, plan, graph, Policy::Eager, runtime, evidence)
        .context("failed to compile execution admission")
}

fn print_schedule_why_origins(
    input: &Path,
    source: &str,
    why: &o_lang::evidence::ScheduleWhyViewV1,
    origins: &[o_lang::parser::SourceSpanV1],
) -> Result<()> {
    use std::collections::BTreeSet;

    let mut referenced = BTreeSet::from([why.operation.plan_node]);
    referenced.extend(
        why.blocker_witnesses
            .iter()
            .map(|witness| witness.predecessor),
    );
    referenced.extend(why.dependents.iter().map(|dependent| dependent.operation));
    referenced.extend(
        why.retained_sequences
            .iter()
            .flat_map(|sequence| [sequence.predecessor, sequence.successor]),
    );

    let source_sha256 = hex::encode(Sha256::digest(source.as_bytes()));
    let path_json = serde_json::to_string(&input.display().to_string())
        .context("failed to render source path for schedule explanation")?;
    println!("; SourceOrigin oexec.source-origin/v1");
    println!(
        "source-binding sha256={} bytes={} path={}",
        source_sha256,
        source.len(),
        path_json
    );
    for plan_node in referenced {
        let origin = origins.get(plan_node.0).with_context(|| {
            format!(
                "source-origin sidecar omits referenced plan operation P{}",
                plan_node.0
            )
        })?;
        println!(
            "source-origin operation=P{} bytes={}..{} start={}:{} end={}:{}",
            plan_node.0,
            origin.start_byte,
            origin.end_byte,
            origin.start_line,
            origin.start_column,
            origin.end_line,
            origin.end_column
        );
    }
    println!(
        "source-origin-note coordinates and source SHA-256 are descriptive sidecar provenance; they are not admission authority and do not alter OIR, plan, graph, or admission digests"
    );
    Ok(())
}

fn inspect_ir(
    source: &str,
) -> Result<(
    OIrProgram,
    o_lang::ir::ExecutionPlan,
    o_lang::hgraph::HGraph,
)> {
    let src = strip_shebang(source);
    let registered_backends = registered_backends();

    let mut parser = Parser::new(&src, &registered_backends);
    let nodes = parser.parse().context("failed to parse .O source")?;

    let program = OIrProgram::lower(&nodes);
    let plan = program.plan();
    let graph = program
        .hgraph_for_plan(&plan)
        .map_err(anyhow::Error::msg)
        .context("failed to build HGraph for IR target")?;
    Ok((program, plan, graph))
}

fn inspect_ir_with_origins(
    source: &str,
) -> Result<(
    OIrProgram,
    o_lang::ir::ExecutionPlan,
    o_lang::hgraph::HGraph,
    Vec<o_lang::parser::SourceSpanV1>,
)> {
    let registered_backends = registered_backends();
    let mut parser = Parser::new(source, &registered_backends);
    let parsed = parser
        .parse_with_origins()
        .context("failed to parse .O source with source origins")?;
    let origins = parsed.plan_origins().to_vec();
    let program = OIrProgram::lower(&parsed.nodes);
    let plan = program.plan();
    let graph = program
        .hgraph_for_plan(&plan)
        .map_err(anyhow::Error::msg)
        .context("failed to build HGraph for IR target")?;
    Ok((program, plan, graph, origins))
}

// ─────────────────────────────────────────────────────────────────────────────
// Target E — emit a Graphviz DOT digraph from the HGraph
//
// Builds the same HGraph that eval.rs uses at runtime, runs the type solver,
// then renders nodes and directed hyperedge ports without flattening away the
// operation boundary. Pipe the output to `dot -Tpng -o out.png` for an image.
// ─────────────────────────────────────────────────────────────────────────────

fn dump_dot(source: &str) -> Result<()> {
    use o_lang::hgraph::solve;

    let src = strip_shebang(source);
    let registered = registered_backends();
    let mut parser = Parser::new(&src, &registered);
    let nodes = parser.parse().context("failed to parse .O source")?;
    let program = OIrProgram::lower(&nodes);
    let plan = program.plan();
    let mut graph = program
        .hgraph_for_plan(&plan)
        .map_err(anyhow::Error::msg)
        .context("failed to build HGraph for DOT target")?;
    solve::solve_types(&mut graph)
        .context("failed to solve HGraph type and fidelity constraints for DOT target")?;
    print!("{}", hgraph_to_dot(&graph));
    Ok(())
}

/// Render the HGraph as Graphviz DOT while preserving hyperedge port direction.
///
/// Every HNode is a DOT vertex whose style identifies whether it carries an
/// ordinary OValue, resource state, persistent actor state, completion, or
/// branch control. Every hyperedge is also a DOT vertex. Input/InOut ports
/// point into that vertex; Output/InOut ports point out. Execute edges use
/// `ExecInfo` so every ordinary and synthetic output is rendered explicitly.
fn hgraph_to_dot(hgraph: &o_lang::hgraph::HGraph) -> String {
    use o_lang::hgraph::{HEdgeKind, PortRole};

    let mut out = String::from(
        "digraph hgraph {\n\
         \x20   graph [bgcolor = \"#11111b\", rankdir = \"LR\"];\n\
         \x20   node [fontname = \"FiraCode\", fontcolor = \"#cdd6f4\"];\n\
         \x20   edge [fontname = \"FiraCode\", fontcolor = \"#bac2de\", color = \"#bac2de\"];\n",
    );

    // Semantic values and state/control tokens.
    for id in hgraph.node_ids() {
        let Some(node) = hgraph.node(id) else {
            continue;
        };
        let (label, style) = hnode_dot_appearance(id, node);
        push_dot_vertex(&mut out, &format!("n{}", id.0), &label, style);
    }

    // Constraint/type relations remain explicit hyperedge vertices. InOut
    // ports receive one arrow in each direction, exactly matching their role.
    for eid in hgraph.edge_ids() {
        let Some(edge) = hgraph.edge(eid) else {
            continue;
        };
        let constraint_id = format!("constraint{}", eid.0);
        let label = format!("Constraint E{}\n{}", eid.0, op_kind_label(&edge.kind));
        push_dot_vertex(&mut out, &constraint_id, &label, DOT_CONSTRAINT_STYLE);

        for port in &edge.ports {
            let node_id = format!("n{}", port.node.0);
            if matches!(port.role, PortRole::Input | PortRole::InOut) {
                push_dot_arrow(&mut out, &node_id, &constraint_id, "in", "constraint");
            }
            if matches!(port.role, PortRole::Output | PortRole::InOut) {
                push_dot_arrow(&mut out, &constraint_id, &node_id, "out", "constraint");
            }
        }
    }

    // Executable hyperedges expose all dependency inputs and every result,
    // successor-state, completion, and control output.
    for info in hgraph.exec_ops_ordered() {
        let Some(edge) = hgraph.exec_edge(info.edge) else {
            continue;
        };
        let HEdgeKind::Execute(op) = &edge.op else {
            continue;
        };
        let exec_id = format!("execute{}", info.edge.0);
        let label = format!(
            "Execute E{}\nP{} ordinal {}\n{}",
            info.edge.0,
            info.plan_node.0,
            info.ordinal,
            executable_op_label(op, info.ready_input_policy(hgraph))
        );
        push_dot_vertex(&mut out, &exec_id, &label, DOT_EXECUTE_STYLE);

        for input in &info.inputs {
            push_dot_arrow(
                &mut out,
                &format!("n{}", input.0),
                &exec_id,
                &format!("input:{}", hnode_port_label(hgraph.node(*input))),
                "execute",
            );
        }
        for output in &info.outputs {
            let role = if *output == info.value_output {
                "result"
            } else {
                hnode_port_label(hgraph.node(*output))
            };
            push_dot_arrow(
                &mut out,
                &exec_id,
                &format!("n{}", output.0),
                role,
                "execute",
            );
        }
    }

    out.push_str("}\n");
    out
}

#[derive(Clone, Copy)]
struct DotVertexStyle {
    shape: &'static str,
    style: &'static str,
    fillcolor: &'static str,
    color: &'static str,
    penwidth: u8,
}

const DOT_VALUE_STYLE: DotVertexStyle = DotVertexStyle {
    shape: "ellipse",
    style: "filled",
    fillcolor: "#1e1e2e",
    color: "#89b4fa",
    penwidth: 1,
};
const DOT_RESOURCE_STYLE: DotVertexStyle = DotVertexStyle {
    shape: "hexagon",
    style: "filled",
    fillcolor: "#243447",
    color: "#74c7ec",
    penwidth: 1,
};
const DOT_ACTOR_STATE_STYLE: DotVertexStyle = DotVertexStyle {
    shape: "doubleoctagon",
    style: "filled,bold",
    fillcolor: "#3b3154",
    color: "#cba6f7",
    penwidth: 2,
};
const DOT_COMPLETION_STYLE: DotVertexStyle = DotVertexStyle {
    shape: "diamond",
    style: "filled",
    fillcolor: "#24352d",
    color: "#a6e3a1",
    penwidth: 1,
};
const DOT_CONTROL_STYLE: DotVertexStyle = DotVertexStyle {
    shape: "octagon",
    style: "filled",
    fillcolor: "#403827",
    color: "#f9e2af",
    penwidth: 1,
};
const DOT_EXECUTE_STYLE: DotVertexStyle = DotVertexStyle {
    shape: "box",
    style: "rounded,filled,bold",
    fillcolor: "#452f3b",
    color: "#f38ba8",
    penwidth: 2,
};
const DOT_CONSTRAINT_STYLE: DotVertexStyle = DotVertexStyle {
    shape: "diamond",
    style: "filled,dashed",
    fillcolor: "#313244",
    color: "#6c7086",
    penwidth: 1,
};

fn hnode_dot_appearance(
    id: o_lang::hgraph::NodeId,
    node: &o_lang::hgraph::HNode,
) -> (String, DotVertexStyle) {
    use o_lang::effects::ResourceKey;
    use o_lang::hgraph::HNodeKind;

    match &node.kind {
        HNodeKind::Value => {
            let mut label = format!("Value N{}", id.0);
            if let Some(value) = &node.value {
                label.push('\n');
                label.push_str(&abbreviate_dot_value(&format!("{value}"), 40));
            }
            if let Some(actor) = node.actor {
                label.push_str(&format!("\nactor({}/{})", actor.lang, actor.env));
            }
            (label, DOT_VALUE_STYLE)
        }
        HNodeKind::ResourceState {
            resource: ResourceKey::ActorState(actor),
            version,
        } => (
            format!("ActorState({actor})@{version}\nN{}", id.0),
            DOT_ACTOR_STATE_STYLE,
        ),
        HNodeKind::ResourceState { resource, version } => (
            format!("{resource}@{version}\nResource N{}", id.0),
            DOT_RESOURCE_STYLE,
        ),
        HNodeKind::Completion { plan_node } => (
            format!("Completion(P{})\nN{}", plan_node.0, id.0),
            DOT_COMPLETION_STYLE,
        ),
        HNodeKind::BranchControl { label, version } => (
            format!("Control({label})@{version}\nN{}", id.0),
            DOT_CONTROL_STYLE,
        ),
        HNodeKind::AdmissionEvidence { plan_node, fact } => (
            format!("Evidence({}:P{})\nN{}", fact.name(), plan_node.0, id.0),
            DOT_CONTROL_STYLE,
        ),
    }
}

fn abbreviate_dot_value(value: &str, max_chars: usize) -> String {
    if value.chars().count() > max_chars {
        format!("{}…", value.chars().take(max_chars).collect::<String>())
    } else {
        value.to_string()
    }
}

fn hnode_port_label(node: Option<&o_lang::hgraph::HNode>) -> &'static str {
    use o_lang::effects::ResourceKey;
    use o_lang::hgraph::HNodeKind;

    match node.map(|node| &node.kind) {
        Some(HNodeKind::Value) => "value",
        Some(HNodeKind::ResourceState {
            resource: ResourceKey::ActorState(_),
            ..
        }) => "actor-state",
        Some(HNodeKind::ResourceState { .. }) => "resource-state",
        Some(HNodeKind::Completion { .. }) => "completion",
        Some(HNodeKind::BranchControl { .. }) => "control",
        Some(HNodeKind::AdmissionEvidence { .. }) => "admission-evidence",
        None => "missing-node",
    }
}

fn push_dot_vertex(out: &mut String, id: &str, label: &str, style: DotVertexStyle) {
    out.push_str(&format!(
        "    {id} [label = \"{}\", shape = \"{}\", style = \"{}\", fillcolor = \"{}\", color = \"{}\", penwidth = \"{}\"];\n",
        dot_escape(label),
        style.shape,
        style.style,
        style.fillcolor,
        style.color,
        style.penwidth,
    ));
}

fn push_dot_arrow(out: &mut String, from: &str, to: &str, label: &str, class: &str) {
    let (color, style) = if class == "constraint" {
        ("#6c7086", "dashed")
    } else {
        ("#f38ba8", "solid")
    };
    out.push_str(&format!(
        "    {from} -> {to} [label = \"{}\", color = \"{color}\", style = \"{style}\"];\n",
        dot_escape(label),
    ));
}

/// Escape a string for use as a DOT label value (inside double quotes).
fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn executable_op_label(
    op: &o_lang::hgraph::ExecutableOp,
    input_policy: Result<o_lang::hgraph::ReadyInputPolicy, String>,
) -> String {
    use o_lang::hgraph::{ExecutableOp, ReadyInputPolicy};

    match (op, input_policy) {
        (ExecutableOp::Store, _) => "store".into(),
        (ExecutableOp::LoadBinding, _) => "load binding".into(),
        (ExecutableOp::Invoke { fn_name, mode }, _) => {
            format!("invoke:{fn_name} ({mode:?})")
        }
        (ExecutableOp::EvalBackend { lang, env }, _) => {
            match o_lang::environment::EnvironmentRefV2::from_encoded(*env) {
                o_lang::environment::EnvironmentRefV2::Ephemeral => format!("eval:{lang}"),
                o_lang::environment::EnvironmentRefV2::LinkerIsolated => format!("eval:{lang}[*]"),
                o_lang::environment::EnvironmentRefV2::Persistent(id) => {
                    format!("eval:{lang}[{id}]")
                }
            }
        }
        (ExecutableOp::InlineBackend { lang }, _) => format!("inline:{lang}"),
        (ExecutableOp::ForceRequest { kind }, _) => format!("force-request:{kind}"),
        (ExecutableOp::Request { kind }, _) => format!("request:{kind}"),
        (ExecutableOp::Group { mode }, _) => format!("group:{mode:?}"),
        (ExecutableOp::Schedule { kind }, _) => format!("schedule:{kind}"),
        (ExecutableOp::MaterializeProject, _) => "materialize-project".into(),
        (ExecutableOp::BuildRoute { route_id }, _) => format!("build-route:{route_id}"),
        (ExecutableOp::RunRoute { route_id }, _) => format!("run-route:{route_id}"),
        (ExecutableOp::SelectRoute { policy }, Ok(ReadyInputPolicy::OrderedFirstSuccess)) => {
            format!("select-route:{policy}\ninputs:ordered-first-success")
        }
        (ExecutableOp::SelectRoute { policy }, Err(_))
            if matches!(policy.as_str(), "fallback" | "any_success") =>
        {
            format!("select-route:{policy}\ninputs:invalid")
        }
        (ExecutableOp::SelectRoute { policy }, _) => format!("select-route:{policy}"),
        (ExecutableOp::CompareRouteResults, _) => "compare-route-results".into(),
    }
}

fn op_kind_label(kind: &o_lang::hgraph::kinds::OpKind) -> String {
    use o_lang::hgraph::kinds::OpKind;
    match kind {
        OpKind::Additive => "additive".into(),
        OpKind::Multiplicative => "multiplicative".into(),
        OpKind::Bitwise => "bitwise".into(),
        OpKind::Ordered => "ordered".into(),
        OpKind::Bounded { .. } => "bounded".into(),
        OpKind::AbiFixed { .. } => "abi_fixed".into(),
        OpKind::Dereferenceable => "deref".into(),
        OpKind::FieldAccess { field } => format!("field:{field}"),
        OpKind::DataFlow => "dataflow".into(),
        OpKind::StructuralBarrier => "structural".into(),
        OpKind::Sequence => "sequence".into(),
        OpKind::ActorSerial { actor } => format!("actor_serial({}/{})", actor.lang, actor.env),
        OpKind::Batch => "batch".into(),
        OpKind::All => "all".into(),
        OpKind::Any => "any".into(),
        OpKind::Race => "race".into(),
        OpKind::Request { kind } => format!("request:{kind}"),
        OpKind::Schedule { kind } => format!("schedule:{kind}"),
        OpKind::CacheMemo { cacheable } => {
            format!("cache:{}", if *cacheable { "memo" } else { "bypass" })
        }
        OpKind::BackendCrossing { from_lang, to_lang } => {
            format!("crossing:{from_lang}→{to_lang}")
        }
        OpKind::X86 { mnemonic } => format!("x86:{mnemonic}"),
        OpKind::OcoreOp { kind } => format!("ocore:{kind:?}"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared front-end helpers
// ─────────────────────────────────────────────────────────────────────────────

/// The backend names accepted in language tags — same set as the O interpreter.
fn registered_backends() -> HashSet<String> {
    // Single source of truth: the central BackendRegistry owns the set of
    // accepted parser tags (canonical names plus aliases).
    o_lang::ir::BackendRegistry::global().registered_backend_tags()
}

/// Drop a leading `#!...` shebang line, if present.
fn strip_shebang(source: &str) -> String {
    if source.starts_with("#!") {
        match source.find('\n') {
            Some(newline) => source[newline + 1..].to_string(),
            None => String::new(),
        }
    } else {
        source.to_string()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Code generation
// ─────────────────────────────────────────────────────────────────────────────

fn generate_lib_rs(include_project: bool) -> String {
    let project_mod = if include_project {
        "pub mod project;\n"
    } else {
        ""
    };
    format!(
        "\
// AUTO-GENERATED by olangc. DO NOT EDIT.
//
// Runtime library crate — all pub items are part of the public API surface,
// so the compiler treats them as reachable regardless of whether the binary
// calls them directly.

pub mod value;
mod capability;
pub mod environment;
pub mod backend;
pub(crate) mod backend_catalog;
pub mod backend_morphism;
pub mod backend_state;
pub mod parser;
#[path = \"placement/protocol/mod.rs\"]
pub(crate) mod placement_protocol;
pub mod placement;
pub mod registry;
pub mod ir;
pub mod effects;
pub mod execution_contract;
pub(crate) mod eval_core;
pub mod evidence;
pub mod hgraph;
pub mod executor;
pub mod eval;
pub mod process;
pub mod nix_ops;
pub mod nixos_ops;
pub mod runtime_exec;
pub mod scheduler;
#[path = \"world/identity.rs\"]
pub mod resource_identity;
pub mod world;
{project_mod}mod canonical_cbor;
mod dispatch_model;
pub mod syntax_dialect;
pub mod wire;
"
    )
}

fn generate_main_rs(
    bin_name: &str,
    program_filename: &str,
    shim_include_lines: &[String],
    backend_grants: &[String],
) -> String {
    let lib_name = generated_lib_name(bin_name);
    let shim_entries = if shim_include_lines.is_empty() {
        "    // no shims bundled".to_string()
    } else {
        shim_include_lines.join("\n")
    };
    let backend_grants = backend_grants
        .iter()
        .map(|grant| format!("    {grant:?},"))
        .collect::<Vec<_>>()
        .join("\n");

    // NOTE: `{{` / `}}` are literal `{` / `}` in a format! string.
    // We use r###"..."### (three hashes) so that `"#` sequences inside the
    // generated code (e.g., `starts_with("#!")`) don't prematurely end the
    // raw-string delimiter.
    format!(
        r###"// AUTO-GENERATED by olangc. DO NOT EDIT.

use {lib_name}::eval::Evaluator;
use {lib_name}::parser::Parser;
use {lib_name}::value::OValue;
use std::collections::HashSet;

/// The compiled .O program source, embedded at compile time.
const PROGRAM_SOURCE: &str = include_str!({program_filename:?});
const BACKEND_GRANTS: &[&str] = &[
{backend_grants}
];

#[cfg(not(target_family = "wasm"))]
/// Backend shim scripts, embedded as raw bytes at compile time.
/// Extracted to a per-invocation temp directory at startup and cleaned up on exit.
const EMBEDDED_SHIMS: &[(&str, &[u8])] = &[
{shim_entries}
];

#[cfg(not(target_family = "wasm"))]
struct ShimGuard(std::path::PathBuf);

#[cfg(not(target_family = "wasm"))]
impl Drop for ShimGuard {{
    fn drop(&mut self) {{
        let _ = std::fs::remove_dir_all(&self.0);
    }}
}}

fn main() -> anyhow::Result<()> {{
    use anyhow::Context as _;

    if {lib_name}::backend::run_backend_from_env_args()? {{
        return Ok(());
    }}

    #[cfg(not(target_family = "wasm"))]
    let shim_dir = {{
        // Extract embedded shims to a private temp directory for this invocation.
        let dir = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(format!(".o_shims_{{}}", std::process::id()));
        std::fs::create_dir_all(&dir)?;

        for (name, content) in EMBEDDED_SHIMS {{
            let dest = dir.join(name);
            std::fs::write(&dest, content)
                .with_context(|| format!("failed to extract shim {{name}}"))?;
            #[cfg(unix)]
            {{
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
            }}
        }}
        dir
    }};
    #[cfg(target_family = "wasm")]
    let shim_dir = std::path::PathBuf::from(".");

    #[cfg(not(target_family = "wasm"))]
    let _guard = ShimGuard(shim_dir.clone());

    let registered_backends: HashSet<String> =
        {lib_name}::ir::BackendRegistry::global().registered_backend_tags();

    let mut source = PROGRAM_SOURCE.to_string();
    if source.starts_with("#!") {{
        if let Some(newline) = source.find('\n') {{
            source = source[newline + 1..].to_string();
        }} else {{
            source.clear();
        }}
    }}

    let mut parser = Parser::new(&source, &registered_backends);
    let nodes = parser.parse().context("failed to parse embedded program")?;

    let mut evaluator = Evaluator::new(shim_dir)
        .with_registered_backends(registered_backends);
    let mut scope = std::collections::HashMap::new();
    for grant in BACKEND_GRANTS {{
        evaluator.install_backend_grant(grant, &mut scope)?;
    }}
    let result = evaluator
        .eval_document_with_scope(nodes, &mut scope)
        .context("failed to evaluate program")?;

    match result {{
        OValue::Html {{ v }} => print!("{{v}}"),
        OValue::Text {{ v }} => print!("{{}}", v.utf8),
        other => println!("{{other}}"),
    }}

    Ok(())
}}
"###,
        lib_name = lib_name,
        program_filename = program_filename,
        shim_entries = shim_entries,
        backend_grants = backend_grants,
    )
}

fn write_generated_cargo_contract(
    build_dir: &Path,
    bin_name: &str,
    include_project: bool,
) -> Result<()> {
    fs::write(
        build_dir.join("Cargo.toml"),
        generate_cargo_toml(bin_name, include_project),
    )?;
    fs::write(
        build_dir.join("Cargo.lock"),
        generate_cargo_lock(include_project)?,
    )?;
    fs::write(
        build_dir.join("rust-toolchain.toml"),
        WORKSPACE_RUST_TOOLCHAIN_TOML,
    )?;
    Ok(())
}

fn generate_cargo_lock(include_project: bool) -> Result<String> {
    let workspace_lock = std::str::from_utf8(WORKSPACE_CARGO_LOCK)
        .context("workspace Cargo.lock embedded in olangc is not UTF-8")?;
    let mut lock: toml::Value =
        toml::from_str(workspace_lock).context("workspace Cargo.lock is not valid TOML")?;

    let package_index = {
        let packages = lock
            .get("package")
            .and_then(toml::Value::as_array)
            .context("workspace Cargo.lock has no package array")?;
        let matches = packages
            .iter()
            .enumerate()
            .filter_map(|(index, package)| {
                let table = package.as_table()?;
                (table.get("name")?.as_str()? == env!("CARGO_PKG_NAME")
                    && table.get("version")?.as_str()? == env!("CARGO_PKG_VERSION")
                    && !table.contains_key("source"))
                .then_some(index)
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [index] => *index,
            _ => bail!(
                "workspace Cargo.lock must contain exactly one local {} {} package entry",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION")
            ),
        }
    };

    {
        let packages = lock
            .get_mut("package")
            .and_then(toml::Value::as_array_mut)
            .context("workspace Cargo.lock has no mutable package array")?;
        let package = packages[package_index]
            .as_table_mut()
            .context("workspace Cargo.lock local package entry is not a table")?;
        let workspace_dependencies = package
            .get("dependencies")
            .and_then(toml::Value::as_array)
            .context("workspace Cargo.lock local package has no dependency list")?
            .iter()
            .map(|dependency| {
                dependency
                    .as_str()
                    .map(str::to_owned)
                    .context("workspace Cargo.lock contains a non-string dependency coordinate")
            })
            .collect::<Result<Vec<_>>>()?;

        let mut dependency_names = GENERATED_RUNTIME_DEPENDENCY_NAMES.to_vec();
        if include_project {
            dependency_names.push("ignore");
        }
        dependency_names.sort_unstable();
        dependency_names.dedup();

        let mut generated_dependencies = Vec::with_capacity(dependency_names.len());
        for name in dependency_names {
            let matches = workspace_dependencies
                .iter()
                .filter(|coordinate| coordinate.split(' ').next() == Some(name))
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [coordinate] => generated_dependencies.push((*coordinate).clone()),
                _ => bail!(
                    "workspace Cargo.lock must contain exactly one direct `{name}` coordinate for generated runtimes"
                ),
            }
        }
        generated_dependencies.sort();

        package.insert(
            "name".to_owned(),
            toml::Value::String(GENERATED_PACKAGE_NAME.to_owned()),
        );
        package.insert(
            "version".to_owned(),
            toml::Value::String(GENERATED_PACKAGE_VERSION.to_owned()),
        );
        package.insert(
            "dependencies".to_owned(),
            toml::Value::Array(
                generated_dependencies
                    .into_iter()
                    .map(toml::Value::String)
                    .collect(),
            ),
        );
        let _ = package.remove("source");
        let _ = package.remove("checksum");
    }

    prune_generated_cargo_lock(&mut lock, package_index)?;

    let serialized = toml::to_string(&lock).context("failed to serialize generated Cargo.lock")?;
    Ok(format!(
        "# This file is @generated by olangc from the Ostadix workspace lock.\n# It is not intended for manual editing.\n{serialized}"
    ))
}

/// Retain the exact transitive package graph reachable from the projected
/// generated root. Cargo treats a copied workspace lock with now-unreachable
/// packages as stale, so root-entry replacement alone is insufficient for a
/// real `--locked` build.
fn prune_generated_cargo_lock(lock: &mut toml::Value, root_index: usize) -> Result<()> {
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .context("generated Cargo.lock has no package array")?;
    let mut reachable = HashSet::new();
    let mut pending = vec![root_index];

    while let Some(index) = pending.pop() {
        if !reachable.insert(index) {
            continue;
        }
        let package = packages
            .get(index)
            .and_then(toml::Value::as_table)
            .context("generated Cargo.lock contains a non-table package")?;
        let dependencies = match package.get("dependencies") {
            Some(value) => value
                .as_array()
                .context("generated Cargo.lock package dependencies are not an array")?,
            None => continue,
        };

        for dependency in dependencies {
            let coordinate = dependency
                .as_str()
                .context("generated Cargo.lock contains a non-string dependency coordinate")?;
            let mut parts = coordinate.split_whitespace();
            let name = parts
                .next()
                .context("generated Cargo.lock contains an empty dependency coordinate")?;
            let version = parts.next();
            if parts.next().is_some() {
                bail!(
                    "generated Cargo.lock dependency coordinate `{coordinate}` has an unsupported source qualifier"
                );
            }

            let matches = packages
                .iter()
                .enumerate()
                .filter_map(|(candidate_index, candidate)| {
                    let candidate = candidate.as_table()?;
                    (candidate.get("name")?.as_str()? == name
                        && version.is_none_or(|expected| {
                            candidate.get("version").and_then(toml::Value::as_str) == Some(expected)
                        }))
                    .then_some(candidate_index)
                })
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [dependency_index] => pending.push(*dependency_index),
                _ => bail!(
                    "generated Cargo.lock dependency `{coordinate}` resolves to {} package entries",
                    matches.len()
                ),
            }
        }
    }

    let packages = lock
        .get_mut("package")
        .and_then(toml::Value::as_array_mut)
        .context("generated Cargo.lock has no mutable package array")?;
    let mut index = 0usize;
    packages.retain(|_| {
        let keep = reachable.contains(&index);
        index += 1;
        keep
    });
    Ok(())
}

fn generate_cargo_toml(bin_name: &str, include_project: bool) -> String {
    // Keep dependency versions in sync with the workspace Cargo.toml.
    // The Cargo.lock (embedded above) pins exact versions, so this just
    // needs to be a compatible range — which the workspace lock already satisfies.
    let ignore_dep = if include_project {
        "ignore     = \"0.4\"\n"
    } else {
        ""
    };
    format!(
        r#"[package]
name    = "{generated_package_name}"
version = "{generated_version}"
edition = "2021"
rust-version = "{package_rust_version}"
publish = false

[package.metadata.ostadix]
embedded-runtime-license = "{package_license}"
embedded-input-license-policy = "retained-by-source"

[lib]
name = "{lib_name}"
path = "src/lib.rs"

[[bin]]
name = "{bin_name}"
path = "src/main.rs"

[dependencies]
serde      = {{ version = "1", features = ["derive"] }}
serde_json = {{ version = "1", features = ["preserve_order"] }}
base64     = "0.22"
toml       = "0.8"
which      = "6"
semver     = {{ version = "1", features = ["serde"] }}
sha2       = "0.10"
hex        = "0.4"
ed25519-dalek = "2"
num-bigint = {{ version = "0.4", features = ["serde"] }}
num-traits = "0.2"
bitflags   = "2"
getrandom  = "0.4.3"
thiserror  = "2"
anyhow     = "1"
clap       = {{ version = "4", features = ["derive"] }}
{ignore_dep}
[target.'cfg(unix)'.dependencies]
libc       = "0.2"

[profile.release]
opt-level     = 3
lto           = "fat"
codegen-units = 1
panic         = "abort"
strip         = "symbols"

[profile.dev.package.sha2]
opt-level = 3
"#,
        bin_name = bin_name,
        lib_name = generated_lib_name(bin_name),
        generated_package_name = GENERATED_PACKAGE_NAME,
        generated_version = GENERATED_PACKAGE_VERSION,
        package_rust_version = env!("CARGO_PKG_RUST_VERSION"),
        package_license = env!("CARGO_PKG_LICENSE"),
        ignore_dep = ignore_dep,
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Create a fresh temporary build directory with a unique name.
fn create_build_dir() -> Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    for _ in 0..64 {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "olang_build_{}_{}_{}",
            std::process::id(),
            timestamp,
            sequence
        ));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    bail!("could not allocate a unique olangc build directory")
}

/// Derive a Cargo-compatible binary name from the output path.
///
/// Cargo allows alphanumerics, hyphens, and underscores in binary names.
/// We replace anything else with `_` and ensure the name doesn't start with
/// a digit (which Cargo rejects as a package name).
fn derive_bin_name(output: &Path) -> String {
    let stem = output
        .file_stem()
        .unwrap_or_else(|| std::ffi::OsStr::new("program"))
        .to_string_lossy()
        .to_string();

    // Sanitize to [a-zA-Z0-9_-]+, starting with a letter or _.
    let sanitized: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{sanitized}")
    } else if sanitized.is_empty() {
        "program".to_string()
    } else {
        sanitized
    }
}

/// Produce a safe fixed filename for the .O source inside the build directory.
///
/// We always use "program.O" regardless of the original filename so the
/// generated main.rs can reference it with a stable literal path.
fn sanitize_program_filename(input_path: &Path) -> String {
    // Keep the extension if it's ".O" (the canonical extension), otherwise
    // use ".O" unconditionally so the name is always predictable.
    let _ = input_path; // original path is accepted for future use
    "program.O".to_string()
}

/// Platform-aware path to the binary produced by `cargo build --release`.
fn built_binary_path(build_dir: &Path, bin_name: &str, is_wasm: bool) -> PathBuf {
    if is_wasm {
        build_dir
            .join("target")
            .join("wasm32-wasip1")
            .join("release")
            .join(format!("{}.wasm", bin_name))
    } else {
        let name = if cfg!(windows) {
            format!("{bin_name}.exe")
        } else {
            bin_name.to_string()
        };
        build_dir.join("target").join("release").join(name)
    }
}

/// Resolve the output path to an absolute path in the current directory.
fn canonicalize_output(output: &Path) -> Result<PathBuf> {
    if output.is_absolute() {
        Ok(output.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("failed to get current directory")?
            .join(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_reports_invalid_effect_declarations_without_panicking() {
        for (source, evidence) in [
            (
                "python{effects=bogus}^(1)_python{effects=bogus}",
                "invalid effect classification `bogus`",
            ),
            (
                "python{effects=pure}^(1)_python{effects=pure}",
                "cannot upgrade an unverified",
            ),
        ] {
            let error = dump_dot(source).unwrap_err();
            let diagnostic = format!("{error:#}");
            assert!(diagnostic.contains("failed to build HGraph for DOT target"));
            assert!(diagnostic.contains(evidence), "{diagnostic}");
        }
    }

    #[test]
    fn ir_reports_unsafe_purity_upgrade_without_panicking() {
        let error = dump_ir("python{effects=pure}^(1)_python{effects=pure}").unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("failed to build HGraph for IR target"));
        assert!(diagnostic.contains("cannot upgrade an unverified"));
    }

    #[test]
    fn grounding_cli_binds_an_exact_nonzero_world_epoch() {
        let cli = Cli::try_parse_from([
            "olangc",
            "demo.O",
            "--target",
            "ir",
            "--grounding",
            "--world-id",
            "desk",
            "--world-epoch",
            "4",
        ])
        .unwrap();
        assert_eq!(
            parse_grounding_world(&cli).unwrap().unwrap().to_string(),
            "desk@4"
        );

        let zero = Cli::try_parse_from([
            "olangc",
            "demo.O",
            "--target",
            "ir",
            "--grounding",
            "--world-id",
            "desk",
            "--world-epoch",
            "0",
        ])
        .unwrap();
        assert!(parse_grounding_world(&zero).is_err());
    }

    #[test]
    fn project_trace_cli_accepts_an_explicit_output_path() {
        let cli = Cli::try_parse_from([
            "olangc",
            "project",
            "--target",
            "script",
            "--project-trace-out",
            "attempt.json",
        ])
        .unwrap();
        assert_eq!(
            cli.project_trace_out.as_deref(),
            Some(Path::new("attempt.json"))
        );
    }

    #[test]
    fn explain_schedule_cli_is_an_ir_only_inspection_surface() {
        let ir = Cli::try_parse_from([
            "olangc",
            "example.O",
            "--target",
            "ir",
            "--explain-schedule",
        ])
        .unwrap();
        assert!(ir.explain_schedule);
        assert_eq!(ir.workers, None);
        validate_admission_inspection(&ir).unwrap();

        let overridden = Cli::try_parse_from([
            "olangc",
            "example.O",
            "--target",
            "ir",
            "--explain-schedule",
            "--workers",
            "3",
        ])
        .unwrap();
        assert_eq!(overridden.workers, Some(3));
        validate_admission_inspection(&overridden).unwrap();

        let zero = Cli::try_parse_from([
            "olangc",
            "example.O",
            "--target",
            "ir",
            "--explain-schedule",
            "--workers",
            "0",
        ])
        .unwrap();
        assert!(validate_admission_inspection(&zero).is_err());

        let without_explanation =
            Cli::try_parse_from(["olangc", "example.O", "--target", "ir", "--workers", "3"])
                .unwrap();
        assert!(validate_admission_inspection(&without_explanation).is_err());

        let script = Cli::try_parse_from([
            "olangc",
            "example.O",
            "--target",
            "script",
            "--explain-schedule",
        ])
        .unwrap();
        assert!(validate_admission_inspection(&script).is_err());
    }

    #[test]
    fn execution_intent_json_cli_is_a_standalone_ir_view() {
        let intent = Cli::try_parse_from([
            "olangc",
            "example.O",
            "--target",
            "ir",
            "--execution-intent-json",
        ])
        .unwrap();
        validate_admission_inspection(&intent).unwrap();

        for args in [
            vec![
                "olangc",
                "example.O",
                "--target",
                "script",
                "--execution-intent-json",
            ],
            vec![
                "olangc",
                "example.O",
                "--target",
                "ir",
                "--execution-intent-json",
                "--explain-schedule",
            ],
            vec![
                "olangc",
                "example.O",
                "--target",
                "ir",
                "--execution-intent-json",
                "--grounding",
            ],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(validate_admission_inspection(&cli).is_err());
        }
    }

    #[test]
    fn why_cli_requires_a_canonical_plan_id_and_ir_only_flags() {
        for (text, expected) in [("P0", 0), ("P7", 7), ("P184", 184)] {
            assert_eq!(
                parse_plan_node_selector(text).unwrap(),
                PlanNodeId(expected)
            );
        }
        for malformed in ["", "P", "p1", "N1", "1", "P01", "P-1", "P 1"] {
            assert!(
                parse_plan_node_selector(malformed).is_err(),
                "accepted malformed selector {malformed:?}"
            );
        }

        let why =
            Cli::try_parse_from(["olangc", "example.O", "--target", "ir", "--why", "P7"]).unwrap();
        assert_eq!(why.why, Some(PlanNodeId(7)));
        validate_admission_inspection(&why).unwrap();

        for args in [
            vec!["olangc", "example.O", "--target", "script", "--why", "P1"],
            vec![
                "olangc",
                "example.O",
                "--target",
                "ir",
                "--why",
                "P1",
                "--explain-schedule",
            ],
            vec![
                "olangc",
                "example.O",
                "--target",
                "ir",
                "--why",
                "P1",
                "--grounding",
            ],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(validate_admission_inspection(&cli).is_err());
        }

        assert!(
            Cli::try_parse_from(["olangc", "example.O", "--target", "ir", "--why", "p1"]).is_err()
        );
    }

    #[test]
    fn why_source_origins_keep_original_shebang_coordinates() {
        let source = "#!/usr/bin/env O\npython^(\n40 + 2\n)_python\n";
        let (program, plan, _graph, origins) = inspect_ir_with_origins(source).unwrap();

        assert_eq!(origins.len(), plan.nodes.len());
        assert_eq!(origins.len(), program.flatten_for_plan().len());
        assert_eq!(
            &source[origins[0].byte_range()],
            "python^(\n40 + 2\n)_python"
        );
        assert_eq!(origins[0].start_line, 2);
        assert_eq!(origins[0].start_column, 1);
    }

    #[test]
    fn generated_runtime_includes_hgraph_modules() {
        let build_dir = create_build_dir().unwrap();
        let src_dir = build_dir.join("src");

        write_runtime_sources(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), generate_lib_rs(false)).unwrap();

        let lib_rs = fs::read_to_string(src_dir.join("lib.rs")).unwrap();
        assert!(lib_rs.contains("pub mod effects;"));
        assert!(lib_rs.contains("pub mod execution_contract;"));
        assert!(lib_rs.contains("pub(crate) mod eval_core;"));
        assert_eq!(
            lib_rs.matches("mod eval_core;").count(),
            1,
            "generated runtimes must compile the evaluator core exactly once"
        );
        assert!(lib_rs.contains("pub mod environment;"));
        assert!(lib_rs.contains("pub(crate) mod backend_catalog;"));
        assert!(lib_rs.contains("pub mod backend_morphism;"));
        assert!(lib_rs.contains("pub mod backend_state;"));
        assert_eq!(
            lib_rs.matches("mod backend_state;").count(),
            1,
            "generated runtimes must compile the backend-state protocol exactly once"
        );
        assert!(lib_rs.contains("#[path = \"placement/protocol/mod.rs\"]"));
        assert!(lib_rs.contains("pub(crate) mod placement_protocol;"));
        assert!(lib_rs.contains("pub mod placement;"));
        assert!(lib_rs.contains("pub mod registry;"));
        assert!(lib_rs.contains("pub mod evidence;"));
        assert!(lib_rs.contains("pub mod hgraph;"));
        assert!(lib_rs.contains("pub mod executor;"));
        assert!(lib_rs.contains("pub mod runtime_exec;"));
        assert!(lib_rs.contains("pub mod resource_identity;"));
        assert!(lib_rs.contains("pub mod world;"));
        assert!(lib_rs.contains("mod canonical_cbor;"));
        assert!(lib_rs.contains("mod dispatch_model;"));
        assert!(lib_rs.contains("pub mod syntax_dialect;"));

        for path in [
            "backend_catalog.rs",
            "backend_catalog.inc.rs",
            "backend_morphism.rs",
            "backend_state.rs",
            "canonical_cbor.rs",
            "dispatch_model.rs",
            "syntax_dialect.rs",
            "environment.rs",
            "effects.rs",
            "execution_contract.rs",
            "eval_core.rs",
            "runtime_exec.rs",
            "placement/mod.rs",
            "placement/projection.rs",
            "placement/protocol/mod.rs",
            "placement/protocol/candidate.rs",
            "placement/protocol/catalog.rs",
            "placement/protocol/digest.rs",
            "placement/protocol/error.rs",
            "placement/protocol/records.rs",
            "placement/protocol/requirement.rs",
            "placement/protocol/state.rs",
            "placement/protocol/target.rs",
            "placement/protocol/warrant.rs",
            "registry/mod.rs",
            "registry/bundle/mod.rs",
            "registry/placement_compat.rs",
            "evidence/mod.rs",
            "evidence/fact.rs",
            "evidence/analyze.rs",
            "evidence/admit.rs",
            "evidence/intent.rs",
            "evidence/profile.rs",
            "world/mod.rs",
            "world/codec.rs",
            "world/identity.rs",
            "world/identity_wire.rs",
            "world/grounding.rs",
            "world/protocol.rs",
            "world/receipt.rs",
            "world/receipt_codec.rs",
            "world/value.rs",
            "world/value_codec.rs",
            "hgraph/mod.rs",
            "hgraph/graph.rs",
            "hgraph/kinds.rs",
            "hgraph/from_oir.rs",
            "hgraph/schedule.rs",
            "hgraph/solve.rs",
            "executor/mod.rs",
            "executor/actor.rs",
            "executor/cancellation.rs",
            "executor/coordinator.rs",
            "executor/effects.rs",
            "executor/parallel.rs",
            "executor/pool.rs",
            "executor/task.rs",
            "executor/trace.rs",
        ] {
            let content = fs::read_to_string(src_dir.join(path)).unwrap();
            assert!(
                !content.trim().is_empty(),
                "generated runtime file {path} must not be empty"
            );
        }
        assert_eq!(
            fs::read_to_string(src_dir.join("backend_catalog.rs")).unwrap(),
            RUNTIME_BACKEND_CATALOG_MODULE_RS,
            "generated runtimes must receive the canonical backend catalog implementation verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("backend_catalog.inc.rs")).unwrap(),
            RUNTIME_BACKEND_CATALOG_DATA_RS,
            "generated runtimes must receive the canonical backend catalog data verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("backend_morphism.rs")).unwrap(),
            RUNTIME_BACKEND_MORPHISM_RS,
            "generated runtimes must receive the bounded backend morphism kernel verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("backend_state.rs")).unwrap(),
            RUNTIME_BACKEND_STATE_RS,
            "generated runtimes must receive the state wire protocol verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("registry/bundle/mod.rs")).unwrap(),
            RUNTIME_REGISTRY_BUNDLE_RS,
            "generated runtimes must receive the canonical backend bundle verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("registry/placement_compat.rs")).unwrap(),
            RUNTIME_REGISTRY_PLACEMENT_COMPAT_RS,
            "generated runtimes must receive catalog/placement integration verbatim"
        );
        let placement_facade = fs::read_to_string(src_dir.join("placement/mod.rs")).unwrap();
        assert!(!placement_facade.contains("pub mod protocol;"));
        assert!(placement_facade.contains("pub mod protocol {"));
        assert!(placement_facade.contains("pub use crate::placement_protocol::*;"));
        for &(relative_path, embedded) in RUNTIME_PLACEMENT_SOURCES {
            assert_eq!(
                fs::read_to_string(src_dir.join("placement").join(relative_path)).unwrap(),
                embedded,
                "generated placement source {relative_path} must match its embedded source"
            );
        }
        assert_eq!(
            fs::read_to_string(src_dir.join("effects.rs")).unwrap(),
            RUNTIME_EFFECTS_RS,
            "generated runtimes must receive the shared semantic effect model verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("execution_contract.rs")).unwrap(),
            RUNTIME_EXECUTION_CONTRACT_RS,
            "generated runtimes must receive the canonical execution contract verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("eval_core.rs")).unwrap(),
            RUNTIME_EVAL_CORE_RS,
            "generated runtimes must receive the evaluator-independent graph contract verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("dispatch_model.rs")).unwrap(),
            RUNTIME_DISPATCH_MODEL_RS,
            "generated runtimes must receive the shared dispatch classification verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("evidence/admit.rs")).unwrap(),
            RUNTIME_EVIDENCE_ADMIT_RS,
            "generated runtimes must receive the admission compiler verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("executor/pool.rs")).unwrap(),
            RUNTIME_EXECUTOR_POOL_RS,
            "generated runtimes must receive the persistent worker pool verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("executor/task.rs")).unwrap(),
            RUNTIME_EXECUTOR_TASK_RS,
            "generated runtimes must receive the prepared-task contract verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("world/identity.rs")).unwrap(),
            RUNTIME_WORLD_IDENTITY_RS,
            "generated runtimes must receive governed identity types verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("world/receipt_codec.rs")).unwrap(),
            RUNTIME_WORLD_RECEIPT_CODEC_RS,
            "generated runtimes must receive the signed receipt codec verbatim"
        );
        assert!(generate_cargo_toml("generated-runtime", false).contains("ed25519-dalek = \"2\""));

        fs::remove_dir_all(build_dir).unwrap();
    }

    #[test]
    fn generated_runtime_manifests_inherit_root_license_policy() {
        for include_project in [false, true] {
            let generated = generate_cargo_toml("generated-runtime", include_project);
            let manifest = toml::from_str::<toml::Value>(&generated)
                .expect("generated Cargo.toml must remain structurally valid");
            let package = manifest
                .get("package")
                .and_then(toml::Value::as_table)
                .expect("generated Cargo.toml must contain a package table");

            assert_eq!(
                package.get("publish").and_then(toml::Value::as_bool),
                Some(false),
                "generated runtimes are build artifacts, not publishable crates"
            );
            assert_eq!(
                package.get("rust-version").and_then(toml::Value::as_str),
                Some(env!("CARGO_PKG_RUST_VERSION")),
                "generated runtimes must retain the package MSRV"
            );
            assert!(
                package.get("license").is_none(),
                "a mixed generated package must not relicense embedded input source"
            );
            let policy = package
                .get("metadata")
                .and_then(toml::Value::as_table)
                .and_then(|metadata| metadata.get("ostadix"))
                .and_then(toml::Value::as_table)
                .expect("generated Cargo.toml must declare component license policy");
            assert_eq!(
                policy
                    .get("embedded-runtime-license")
                    .and_then(toml::Value::as_str),
                Some(env!("CARGO_PKG_LICENSE")),
                "generated runtimes must identify the embedded runtime license"
            );
            assert_eq!(
                policy
                    .get("embedded-input-license-policy")
                    .and_then(toml::Value::as_str),
                Some("retained-by-source"),
                "generated runtimes must preserve the input source's license policy"
            );
        }
    }

    #[test]
    fn generated_runtime_build_contract_projects_lock_and_toolchain() {
        for include_project in [false, true] {
            for bin_name in ["generated-runtime", "serde"] {
                let build_dir = tempfile::tempdir().unwrap();
                write_generated_cargo_contract(build_dir.path(), bin_name, include_project)
                    .unwrap();

                assert_eq!(
                    fs::read(build_dir.path().join("rust-toolchain.toml")).unwrap(),
                    WORKSPACE_RUST_TOOLCHAIN_TOML,
                    "generated builds must inherit the authoritative compiler pin"
                );

                let manifest = fs::read_to_string(build_dir.path().join("Cargo.toml")).unwrap();
                let manifest = toml::from_str::<toml::Value>(&manifest).unwrap();
                let package = manifest
                    .get("package")
                    .and_then(toml::Value::as_table)
                    .unwrap();
                let expected_lib_name = generated_lib_name(bin_name);
                assert_eq!(
                    package.get("name").and_then(toml::Value::as_str),
                    Some(GENERATED_PACKAGE_NAME)
                );
                assert_eq!(
                    manifest
                        .get("lib")
                        .and_then(toml::Value::as_table)
                        .and_then(|lib| lib.get("name"))
                        .and_then(toml::Value::as_str),
                    Some(expected_lib_name.as_str())
                );

                let lock_text = fs::read_to_string(build_dir.path().join("Cargo.lock")).unwrap();
                assert_eq!(
                    lock_text,
                    generate_cargo_lock(include_project).unwrap(),
                    "generated lock projection must be deterministic and output-name independent"
                );
                let lock = toml::from_str::<toml::Value>(&lock_text).unwrap();
                let packages = lock.get("package").and_then(toml::Value::as_array).unwrap();
                let generated = packages
                    .iter()
                    .filter_map(toml::Value::as_table)
                    .filter(|package| {
                        package.get("name").and_then(toml::Value::as_str)
                            == Some(GENERATED_PACKAGE_NAME)
                            && package.get("source").is_none()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(generated.len(), 1);
                assert_eq!(
                    generated[0].get("version").and_then(toml::Value::as_str),
                    Some(GENERATED_PACKAGE_VERSION)
                );

                let actual_dependencies = generated[0]
                    .get("dependencies")
                    .and_then(toml::Value::as_array)
                    .unwrap()
                    .iter()
                    .map(|dependency| dependency.as_str().unwrap().split(' ').next().unwrap())
                    .collect::<HashSet<_>>();
                let mut expected_dependencies = GENERATED_RUNTIME_DEPENDENCY_NAMES
                    .iter()
                    .copied()
                    .collect::<HashSet<_>>();
                if include_project {
                    expected_dependencies.insert("ignore");
                }
                assert_eq!(actual_dependencies, expected_dependencies);
            }
        }
    }

    #[test]
    fn generated_project_runtime_emits_every_embedded_source() {
        let build_dir = tempfile::tempdir().unwrap();
        let src_dir = build_dir.path().join("src");
        write_project_sources(&src_dir).unwrap();
        for &(relative_path, embedded) in RUNTIME_PROJECT_SOURCES {
            let emitted = fs::read_to_string(src_dir.join("project").join(relative_path))
                .unwrap_or_else(|error| panic!("missing generated {relative_path}: {error}"));
            assert_eq!(
                emitted, embedded,
                "generated project source {relative_path} must match its embedded source"
            );
        }
    }

    #[test]
    fn generated_project_runtime_compiles_real_fixture() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_hgraph");
        let bundle = o_lang::project::assemble(&fixture, "generated-project-closure", &[])
            .expect("real project fixture must assemble");
        let build_dir = tempfile::tempdir().unwrap();
        write_project_cargo_project(&bundle, &[], build_dir.path(), "serde")
            .expect("real project fixture must generate a Cargo project");

        let probe_dir = build_dir.path().join("tests");
        fs::create_dir_all(&probe_dir).unwrap();
        fs::write(
            probe_dir.join("generated_runtime_closure.rs"),
            r#"use ostadix_generated_serde::backend::state::{
    empty_checkpoint, validate_empty_restore, BackendStateTierV1 as CompatibilityBackendStateTierV1,
};
use ostadix_generated_serde::backend_state::BackendStateTierV1 as CanonicalBackendStateTierV1;
use ostadix_generated_serde::placement::SemanticDigestV1;
use ostadix_generated_serde::placement::protocol::SemanticDigestV1 as NestedSemanticDigestV1;
use ostadix_generated_serde::execution_contract::Policy as CanonicalPolicy;
use ostadix_generated_serde::eval::Policy as CompatibilityPolicy;
use ostadix_generated_serde::evidence::{
    admit_execution_v6, analyze_execution, analyze_execution_v6, graph_sha256_v1,
    graph_sha256_v2, runtime_binding_from_adapter_bytes, ADMISSION_SCHEMA_V6,
    EVIDENCE_SCHEMA_V5, EVIDENCE_SCHEMA_V6, PLACEMENT_ADMISSION_DIGEST_DOMAIN_V2,
    SCHEDULE_WHY_SCHEMA_V2, SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V2,
};
use ostadix_generated_serde::ir::{OIr, OIrProgram};
use ostadix_generated_serde::registry::bundle::{
    BackendMorphismProfileV1, BackendRegistry, IntegerExactness,
    BACKEND_CATALOG_CURRENT_SCHEMA, BACKEND_CATALOG_SCHEMA_V4, BACKEND_CATALOG_SCHEMA_V5,
};
use ostadix_generated_serde::{resource_identity, world};

#[test]
fn catalog_placement_and_checkpoint_sources_are_live() {
    assert_eq!(BACKEND_CATALOG_CURRENT_SCHEMA, BACKEND_CATALOG_SCHEMA_V5);
    assert_eq!(BACKEND_CATALOG_SCHEMA_V4, "ostadix.backend-catalog/v4");
    let rust = BackendRegistry::global().interface_for("rust");
    assert!(matches!(
        rust.value_capabilities.integer_exactness,
        IntegerExactness::TwosComplementBits(63)
    ));
    assert_eq!(
        BackendRegistry::global().morphism_profile_for("rust"),
        Some(BackendMorphismProfileV1::RustSourceConstantStdout)
    );

    let runtime = SemanticDigestV1::hash_bytes(
        "ostadix/generated-runtime-closure-test/v1",
        b"rust-runtime",
    );
    let checkpoint = empty_checkpoint("rust", runtime.as_sha256()).unwrap();
    checkpoint.validate().unwrap();
    validate_empty_restore("rust", runtime.as_sha256(), &checkpoint).unwrap();
}

#[test]
fn backend_state_root_and_legacy_paths_share_one_type_identity() {
    let canonical = CanonicalBackendStateTierV1::SemanticSnapshot;
    let compatibility: CompatibilityBackendStateTierV1 = canonical;
    let canonical_again: CanonicalBackendStateTierV1 = compatibility;
    assert_eq!(canonical_again, CanonicalBackendStateTierV1::SemanticSnapshot);
}

#[test]
fn flat_and_nested_placement_paths_share_one_type_identity() {
    let nested = NestedSemanticDigestV1::hash_bytes(
        "ostadix/generated-runtime-placement-alias/v1",
        b"one-canonical-module",
    );
    let flat: SemanticDigestV1 = nested;
    let nested_again: NestedSemanticDigestV1 = flat;
    assert_eq!(nested_again.as_sha256().len(), 64);
}

#[test]
fn world_artifact_id_is_the_shared_identity_in_generated_aot_runtime() {
    let shared = resource_identity::ArtifactId::from_sha256("ab".repeat(32)).unwrap();
    let through_world: world::ArtifactId = shared.clone();
    let through_world_identity_module: world::identity::ArtifactId = shared.clone();
    let shared_again: resource_identity::ArtifactId = through_world;

    assert_eq!(shared_again, shared);
    assert_eq!(through_world_identity_module, shared);
}

#[test]
fn execution_policy_is_one_type_in_generated_aot_runtime() {
    let compatibility: CompatibilityPolicy = CanonicalPolicy::Autonomous;
    let canonical: CanonicalPolicy = compatibility;
    assert_eq!(canonical.name(), "autonomous");
    assert_eq!(CanonicalPolicy::from_name(canonical.name()), Some(canonical));
}

#[test]
fn explicit_evidence_v6_symbols_admit_a_real_generated_runtime_fixture() {
    let program = OIrProgram {
        nodes: vec![OIr::Load("generated-runtime-input".to_string())],
    };
    let plan = program.plan();
    let mut graph = program.hgraph_for_plan(&plan).unwrap();
    ostadix_generated_serde::hgraph::solve::solve_types(&mut graph).unwrap();
    let runtime = runtime_binding_from_adapter_bytes(
        &plan,
        &[],
        &[("generated-runtime-v6", "explicit")],
    );

    let current = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
    assert_eq!(current.schema(), EVIDENCE_SCHEMA_V5);
    let v6 = analyze_execution_v6(&program, &plan, &graph, runtime.clone()).unwrap();
    assert_eq!(v6.schema(), EVIDENCE_SCHEMA_V6);
    assert_eq!(v6.bindings().analyzed_graph_sha256, graph_sha256_v2(&graph));
    assert_ne!(graph_sha256_v1(&graph), graph_sha256_v2(&graph));

    let admitted = admit_execution_v6(
        &program,
        &plan,
        graph,
        CanonicalPolicy::Eager,
        runtime,
        v6,
    )
    .unwrap();
    assert_eq!(admitted.admission().schema(), ADMISSION_SCHEMA_V6);
    assert_eq!(
        SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V2,
        "ostadix-solved-executable-hgraph/v2"
    );
    assert_eq!(
        PLACEMENT_ADMISSION_DIGEST_DOMAIN_V2,
        "ostadix/placement-admission/v2"
    );
    let target = admitted.admission().operations()[0].plan_node;
    assert_eq!(admitted.schedule_why(target).unwrap().schema, SCHEDULE_WHY_SCHEMA_V2);
}
"#,
        )
        .unwrap();

        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let output = Command::new(cargo)
            .args(["check", "--offline", "--locked", "--color", "never"])
            .env("CARGO_TARGET_DIR", build_dir.path().join("target"))
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_PROFILE_DEV_DEBUG", "0")
            .env("CARGO_PROFILE_TEST_DEBUG", "0")
            .current_dir(build_dir.path())
            .output()
            .expect("Cargo must be available to check the generated project");
        assert!(
            output.status.success(),
            "generated project failed cargo check\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let output = Command::new(cargo)
            .args([
                "test",
                "--offline",
                "--locked",
                "--color",
                "never",
                "--test",
                "generated_runtime_closure",
            ])
            .env("CARGO_TARGET_DIR", build_dir.path().join("target"))
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_PROFILE_DEV_DEBUG", "0")
            .env("CARGO_PROFILE_TEST_DEBUG", "0")
            .current_dir(build_dir.path())
            .output()
            .expect("Cargo must be available to exercise the generated project closure");
        assert!(
            output.status.success(),
            "generated project closure probe failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn generated_project_main_handles_project_crate_name() {
        let main = generate_project_main_rs("project", &[]);
        assert!(main.contains("use ::ostadix_generated_project::project::RoutePolicy;"));
        assert!(main.contains("::ostadix_generated_project::project::bundle::deserialize"));
        assert!(!main.contains("use project::project::{self"));
        assert!(main.contains("--route requires a value"));
        assert!(main.contains("RoutePolicy::parse_checked"));
        assert!(main.contains("--project-trace-out requires a path"));
        assert!(main.contains("execute_selection_with_configured_executor"));
        assert!(main.contains("write_project_attempt_trace"));
        assert!(main.contains("ProjectExecutionError"));
    }

    #[test]
    fn dot_renders_directed_execute_ports_and_distinct_synthetic_nodes() {
        use o_lang::effects::ResourceKey;
        use o_lang::hgraph::{HNode, HNodeKind};
        use o_lang::ir::{BackendRegistry, OIr};

        let python = BackendRegistry::global().interface_for("python");
        let program = OIrProgram {
            nodes: vec![
                OIr::Exec {
                    lang: "python".into(),
                    env_id: 0,
                    attr: None,
                    backend: python.clone(),
                    body: vec![OIr::Text("first".into())],
                },
                OIr::Exec {
                    lang: "python".into(),
                    env_id: 0,
                    attr: None,
                    backend: python,
                    body: vec![OIr::Text("second".into())],
                },
            ],
        };
        let mut graph = program.hgraph();
        let control = graph.add_node(HNode::branch_control("then", 0, true));
        let dot = hgraph_to_dot(&graph);

        let value = graph
            .node_ids()
            .into_iter()
            .find(|id| {
                matches!(
                    graph.node(*id).map(|node| &node.kind),
                    Some(HNodeKind::Value)
                )
            })
            .expect("graph has an ordinary value");
        let host_world = graph
            .node_ids()
            .into_iter()
            .find(|id| {
                matches!(
                    graph.node(*id).map(|node| &node.kind),
                    Some(HNodeKind::ResourceState {
                        resource: ResourceKey::HostWorld,
                        ..
                    })
                )
            })
            .expect("unknown Python execution has HostWorld state");
        let actor_state = graph
            .node_ids()
            .into_iter()
            .find(|id| {
                matches!(
                    graph.node(*id).map(|node| &node.kind),
                    Some(HNodeKind::ResourceState {
                        resource: ResourceKey::ActorState(_),
                        ..
                    })
                )
            })
            .expect("persistent Python execution has actor state");
        let completion = graph
            .node_ids()
            .into_iter()
            .find(|id| {
                matches!(
                    graph.node(*id).map(|node| &node.kind),
                    Some(HNodeKind::Completion { .. })
                )
            })
            .expect("execute edge has a completion output");

        assert_dot_vertex_style(&dot, &format!("n{}", value.0), "ellipse", "#89b4fa");
        assert_dot_vertex_style(&dot, &format!("n{}", host_world.0), "hexagon", "#74c7ec");
        assert_dot_vertex_style(
            &dot,
            &format!("n{}", actor_state.0),
            "doubleoctagon",
            "#cba6f7",
        );
        assert_dot_vertex_style(&dot, &format!("n{}", completion.0), "diamond", "#a6e3a1");
        assert_dot_vertex_style(&dot, &format!("n{}", control.0), "octagon", "#f9e2af");
        assert!(dot.contains("HostWorld@0"));
        assert!(dot.contains("ActorState(python[0])@0"));
        assert!(dot.contains("Completion(P"));
        assert!(dot.contains("Control(then)@0"));

        let infos = graph.exec_ops_ordered();
        assert_eq!(infos.len(), 2, "the two Python blocks are executable edges");
        for info in infos {
            let execute = format!("execute{}", info.edge.0);
            assert_dot_vertex_style(&dot, &execute, "box", "#f38ba8");

            for input in &info.inputs {
                let arrow = format!("n{} -> {execute} [", input.0);
                assert_eq!(
                    dot.matches(&arrow).count(),
                    1,
                    "each execute input must point into its operation vertex: {arrow}"
                );
            }
            for output in &info.outputs {
                let arrow = format!("{execute} -> n{} [", output.0);
                assert_eq!(
                    dot.matches(&arrow).count(),
                    1,
                    "each execute output must receive one arrow from its operation vertex: {arrow}"
                );
            }
        }

        let constraint = graph
            .edge_ids()
            .into_iter()
            .next()
            .expect("lowering has constraint hyperedges");
        assert_dot_vertex_style(
            &dot,
            &format!("constraint{}", constraint.0),
            "diamond",
            "#6c7086",
        );
    }

    #[test]
    fn dot_preserves_inout_port_direction() {
        use o_lang::hgraph::{HEdge, HGraph, HNode, OpKind, Port, PortRole};

        let mut graph = HGraph::default();
        let node = graph.add_node(HNode::with_value(OValue::Null));
        let edge = graph.add_edge(HEdge::constraint(
            OpKind::Additive,
            vec![Port {
                node,
                role: PortRole::InOut,
            }],
        ));
        let dot = hgraph_to_dot(&graph);

        assert!(dot.contains(&format!("n{} -> constraint{} [", node.0, edge.0)));
        assert!(dot.contains(&format!("constraint{} -> n{} [", edge.0, node.0)));
    }

    fn assert_dot_vertex_style(dot: &str, id: &str, shape: &str, color: &str) {
        let prefix = format!("    {id} [");
        let line = dot
            .lines()
            .find(|line| line.starts_with(&prefix))
            .unwrap_or_else(|| panic!("missing DOT vertex {id}"));
        assert!(
            line.contains(&format!("shape = \"{shape}\"")),
            "DOT vertex {id} has wrong shape: {line}"
        );
        assert!(
            line.contains(&format!("color = \"{color}\"")),
            "DOT vertex {id} has wrong color: {line}"
        );
    }
}
