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
//        - The workspace Cargo.lock so dependency resolution is instant and
//          reproducible (embedded in olangc at its own compile time).
//   4. Runs `cargo build --release` in the temp project.
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
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use o_lang::eval::Evaluator;
use o_lang::ir::OIrProgram;
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
const RUNTIME_PARSER_RS: &str = include_str!("../parser.rs");
const RUNTIME_IR_RS: &str = include_str!("../ir.rs");
const RUNTIME_EVAL_RS: &str = include_str!("../eval.rs");
const RUNTIME_PROCESS_RS: &str = include_str!("../process.rs");
const RUNTIME_BACKEND_RS: &str = include_str!("../backend.rs");
const RUNTIME_NIX_OPS_RS: &str = include_str!("../nix_ops.rs");
const RUNTIME_NIXOS_OPS_RS: &str = include_str!("../nixos_ops.rs");
const RUNTIME_SCHEDULER_RS: &str = include_str!("../scheduler.rs");
const RUNTIME_WIRE_RS: &str = include_str!("../wire.rs");
const RUNTIME_EFFECTS_RS: &str = include_str!("../effects.rs");

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
const RUNTIME_EXECUTOR_TRACE_RS: &str = include_str!("../executor/trace.rs");

// project — first-class project/route/bundle model, embedded so compiled
// project binaries can materialize and run their embedded routes.
const RUNTIME_PROJECT_MOD_RS: &str = include_str!("../project/mod.rs");
const RUNTIME_PROJECT_MODEL_RS: &str = include_str!("../project/model.rs");
const RUNTIME_PROJECT_BUNDLE_RS: &str = include_str!("../project/bundle.rs");
const RUNTIME_PROJECT_MATERIALIZE_RS: &str = include_str!("../project/materialize.rs");
const RUNTIME_PROJECT_MANIFEST_RS: &str = include_str!("../project/manifest.rs");
const RUNTIME_PROJECT_DISCOVER_RS: &str = include_str!("../project/discover.rs");
const RUNTIME_PROJECT_LOWER_RS: &str = include_str!("../project/lower.rs");
const RUNTIME_PROJECT_PLAN_RS: &str = include_str!("../project/plan.rs");
const RUNTIME_PROJECT_EXECUTOR_RS: &str = include_str!("../project/executor.rs");
const RUNTIME_PROJECT_RUNTIME_RS: &str = include_str!("../project/runtime.rs");
const RUNTIME_PROJECT_TRACE_RS: &str = include_str!("../project/trace.rs");
const RUNTIME_PROJECT_ECOSYSTEMS_MOD_RS: &str = include_str!("../project/ecosystems/mod.rs");
const RUNTIME_PROJECT_ECO_PYTHON_RS: &str = include_str!("../project/ecosystems/python.rs");
const RUNTIME_PROJECT_ECO_JAVASCRIPT_RS: &str = include_str!("../project/ecosystems/javascript.rs");
const RUNTIME_PROJECT_ECO_RUST_RS: &str = include_str!("../project/ecosystems/rust.rs");
const RUNTIME_PROJECT_ECO_SHELL_RS: &str = include_str!("../project/ecosystems/shell.rs");
const RUNTIME_PROJECT_ECO_GENERIC_RS: &str = include_str!("../project/ecosystems/generic.rs");
const RUNTIME_PROJECT_ECO_C_FAMILY_RS: &str = include_str!("../project/ecosystems/c_family.rs");
const RUNTIME_PROJECT_ECO_JAVA_RS: &str = include_str!("../project/ecosystems/java.rs");
const RUNTIME_PROJECT_ECO_DOTNET_RS: &str = include_str!("../project/ecosystems/dotnet.rs");
const RUNTIME_PROJECT_ECO_HASKELL_OCAML_RS: &str =
    include_str!("../project/ecosystems/haskell_ocaml.rs");
const RUNTIME_PROJECT_ECO_NIX_RS: &str = include_str!("../project/ecosystems/nix.rs");

// Cargo.lock from the workspace — embedded so the temp project gets identical
// dependency versions on first build without a network round-trip.
const WORKSPACE_CARGO_LOCK: &[u8] = include_bytes!("../../Cargo.lock");

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
        CompileTarget::Ir if cli.grounding => dump_ir_with_grounding(&source, grounding_world),
        CompileTarget::Ir => dump_ir(&source),
        CompileTarget::Dot => dump_dot(&source),
    }
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
            print!("{}", project.to_text());
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
    let main_rs = generate_project_main_rs(&bin_name, &shim_include_lines);
    fs::write(src_dir.join("main.rs"), &main_rs)?;

    fs::write(
        build_dir.join("Cargo.toml"),
        generate_cargo_toml(&bin_name, true),
    )?;
    fs::write(build_dir.join("Cargo.lock"), WORKSPACE_CARGO_LOCK)?;

    let mut cargo_args = vec!["build", "--release"];
    if is_wasm {
        cargo_args.push("--target");
        cargo_args.push("wasm32-wasip1");
        eprintln!("olangc: running cargo build --release --target wasm32-wasip1 ...");
    } else {
        eprintln!("olangc: running cargo build --release ...");
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

/// Write the embedded `project` module tree into the generated `src/`.
fn write_project_sources(src_dir: &Path) -> Result<()> {
    let project_dir = src_dir.join("project");
    let eco_dir = project_dir.join("ecosystems");
    fs::create_dir_all(&eco_dir)?;

    fs::write(project_dir.join("mod.rs"), RUNTIME_PROJECT_MOD_RS)?;
    fs::write(project_dir.join("model.rs"), RUNTIME_PROJECT_MODEL_RS)?;
    fs::write(project_dir.join("bundle.rs"), RUNTIME_PROJECT_BUNDLE_RS)?;
    fs::write(
        project_dir.join("materialize.rs"),
        RUNTIME_PROJECT_MATERIALIZE_RS,
    )?;
    fs::write(project_dir.join("manifest.rs"), RUNTIME_PROJECT_MANIFEST_RS)?;
    fs::write(project_dir.join("discover.rs"), RUNTIME_PROJECT_DISCOVER_RS)?;
    fs::write(project_dir.join("lower.rs"), RUNTIME_PROJECT_LOWER_RS)?;
    fs::write(project_dir.join("plan.rs"), RUNTIME_PROJECT_PLAN_RS)?;
    fs::write(project_dir.join("executor.rs"), RUNTIME_PROJECT_EXECUTOR_RS)?;
    fs::write(project_dir.join("runtime.rs"), RUNTIME_PROJECT_RUNTIME_RS)?;
    fs::write(project_dir.join("trace.rs"), RUNTIME_PROJECT_TRACE_RS)?;

    fs::write(eco_dir.join("mod.rs"), RUNTIME_PROJECT_ECOSYSTEMS_MOD_RS)?;
    fs::write(eco_dir.join("python.rs"), RUNTIME_PROJECT_ECO_PYTHON_RS)?;
    fs::write(
        eco_dir.join("javascript.rs"),
        RUNTIME_PROJECT_ECO_JAVASCRIPT_RS,
    )?;
    fs::write(eco_dir.join("rust.rs"), RUNTIME_PROJECT_ECO_RUST_RS)?;
    fs::write(eco_dir.join("shell.rs"), RUNTIME_PROJECT_ECO_SHELL_RS)?;
    fs::write(eco_dir.join("generic.rs"), RUNTIME_PROJECT_ECO_GENERIC_RS)?;
    fs::write(eco_dir.join("c_family.rs"), RUNTIME_PROJECT_ECO_C_FAMILY_RS)?;
    fs::write(eco_dir.join("java.rs"), RUNTIME_PROJECT_ECO_JAVA_RS)?;
    fs::write(eco_dir.join("dotnet.rs"), RUNTIME_PROJECT_ECO_DOTNET_RS)?;
    fs::write(
        eco_dir.join("haskell_ocaml.rs"),
        RUNTIME_PROJECT_ECO_HASKELL_OCAML_RS,
    )?;
    fs::write(eco_dir.join("nix.rs"), RUNTIME_PROJECT_ECO_NIX_RS)?;
    Ok(())
}

/// Generate the `main.rs` for a compiled project binary. It embeds the bundle
/// and supports `--list-routes`, `--route <ID>`, `--routes-policy <POLICY>`,
/// `--project-trace-out <PATH>`, and default-route execution.
fn generate_project_main_rs(bin_name: &str, shim_include_lines: &[String]) -> String {
    let lib_name = bin_name.replace('-', "_");
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

    // ── Cargo.toml ───────────────────────────────────────────────────────────
    fs::write(
        build_dir.join("Cargo.toml"),
        generate_cargo_toml(&bin_name, false),
    )?;

    // ── Cargo.lock — embed workspace lock for reproducible/fast first build ──
    fs::write(build_dir.join("Cargo.lock"), WORKSPACE_CARGO_LOCK)?;

    // ── Build ────────────────────────────────────────────────────────────────
    let mut cargo_args = vec!["build", "--release"];
    if is_wasm {
        cargo_args.push("--target");
        cargo_args.push("wasm32-wasip1");
        eprintln!("olangc: running cargo build --release --target wasm32-wasip1 ...");
    } else {
        eprintln!("olangc: running cargo build --release ...");
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
    fs::write(src_dir.join("parser.rs"), RUNTIME_PARSER_RS)?;
    fs::write(src_dir.join("ir.rs"), RUNTIME_IR_RS)?;
    fs::write(src_dir.join("eval.rs"), RUNTIME_EVAL_RS)?;
    fs::write(src_dir.join("process.rs"), RUNTIME_PROCESS_RS)?;
    fs::write(src_dir.join("backend.rs"), RUNTIME_BACKEND_RS)?;
    fs::write(src_dir.join("nix_ops.rs"), RUNTIME_NIX_OPS_RS)?;
    fs::write(src_dir.join("nixos_ops.rs"), RUNTIME_NIXOS_OPS_RS)?;
    fs::write(src_dir.join("scheduler.rs"), RUNTIME_SCHEDULER_RS)?;
    fs::write(src_dir.join("wire.rs"), RUNTIME_WIRE_RS)?;
    fs::write(src_dir.join("effects.rs"), RUNTIME_EFFECTS_RS)?;

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
    solve::solve_types(&mut graph);
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
            executable_op_label(op)
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

fn executable_op_label(op: &o_lang::hgraph::ExecutableOp) -> String {
    use o_lang::hgraph::ExecutableOp;

    match op {
        ExecutableOp::Store => "store".into(),
        ExecutableOp::LoadBinding => "load binding".into(),
        ExecutableOp::Invoke { fn_name, mode } => format!("invoke:{fn_name} ({mode:?})"),
        ExecutableOp::EvalBackend { lang, env } if *env == u32::MAX => {
            format!("eval:{lang}")
        }
        ExecutableOp::EvalBackend { lang, env } => format!("eval:{lang}[{env}]"),
        ExecutableOp::InlineBackend { lang } => format!("inline:{lang}"),
        ExecutableOp::ForceRequest { kind } => format!("force-request:{kind}"),
        ExecutableOp::Request { kind } => format!("request:{kind}"),
        ExecutableOp::Group { mode } => format!("group:{mode:?}"),
        ExecutableOp::Schedule { kind } => format!("schedule:{kind}"),
        ExecutableOp::MaterializeProject => "materialize-project".into(),
        ExecutableOp::BuildRoute { route_id } => format!("build-route:{route_id}"),
        ExecutableOp::RunRoute { route_id } => format!("run-route:{route_id}"),
        ExecutableOp::SelectRoute { policy } => format!("select-route:{policy}"),
        ExecutableOp::CompareRouteResults => "compare-route-results".into(),
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
pub mod backend;
pub mod parser;
pub mod ir;
pub mod effects;
pub mod hgraph;
pub mod executor;
pub mod eval;
pub mod process;
pub mod nix_ops;
pub mod nixos_ops;
pub mod scheduler;
pub mod world;
{project_mod}pub(crate) mod wire;
"
    )
}

fn generate_main_rs(
    bin_name: &str,
    program_filename: &str,
    shim_include_lines: &[String],
    backend_grants: &[String],
) -> String {
    let lib_name = bin_name.replace('-', "_");
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
name    = "{bin_name}"
version = "0.1.0"
edition = "2021"

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
"#,
        bin_name = bin_name,
        lib_name = bin_name.replace('-', "_"),
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
    fn generated_runtime_includes_hgraph_modules() {
        let build_dir = create_build_dir().unwrap();
        let src_dir = build_dir.join("src");

        write_runtime_sources(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), generate_lib_rs(false)).unwrap();

        let lib_rs = fs::read_to_string(src_dir.join("lib.rs")).unwrap();
        assert!(lib_rs.contains("pub mod effects;"));
        assert!(lib_rs.contains("pub mod hgraph;"));
        assert!(lib_rs.contains("pub mod executor;"));
        assert!(lib_rs.contains("pub mod world;"));

        for path in [
            "effects.rs",
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
            "executor/trace.rs",
        ] {
            let content = fs::read_to_string(src_dir.join(path)).unwrap();
            assert!(
                !content.trim().is_empty(),
                "generated runtime file {path} must not be empty"
            );
        }
        assert_eq!(
            fs::read_to_string(src_dir.join("effects.rs")).unwrap(),
            RUNTIME_EFFECTS_RS,
            "generated runtimes must receive the shared semantic effect model verbatim"
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
    fn generated_project_runtime_includes_project_plan_module() {
        let build_dir = create_build_dir().unwrap();
        let src_dir = build_dir.join("src");
        write_project_sources(&src_dir).unwrap();
        assert_eq!(
            fs::read_to_string(src_dir.join("project/plan.rs")).unwrap(),
            RUNTIME_PROJECT_PLAN_RS,
            "compiled project runtimes must receive the project HGraph planner verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("project/executor.rs")).unwrap(),
            RUNTIME_PROJECT_EXECUTOR_RS,
            "compiled project runtimes must receive the project HGraph executor verbatim"
        );
        assert_eq!(
            fs::read_to_string(src_dir.join("project/trace.rs")).unwrap(),
            RUNTIME_PROJECT_TRACE_RS,
            "compiled project runtimes must receive the project attempt trace verbatim"
        );
        let module = fs::read_to_string(src_dir.join("project/mod.rs")).unwrap();
        for declaration in ["pub mod plan;", "pub mod executor;", "pub mod trace;"] {
            assert!(module.contains(declaration), "missing `{declaration}`");
        }
        fs::remove_dir_all(build_dir).unwrap();
    }

    #[test]
    fn generated_project_main_handles_project_crate_name() {
        let main = generate_project_main_rs("project", &[]);
        assert!(main.contains("use ::project::project::RoutePolicy;"));
        assert!(main.contains("::project::project::bundle::deserialize"));
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
