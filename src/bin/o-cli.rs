//! Compiled intent-oriented front door for the repository-owned `o` command.
//!
//! The Bash dispatcher retains legacy command routing and evaluator fallthrough,
//! but sends `run`, `plan`, `explain`, and `inspect` here so their grammar is
//! defined once. Execution and planning call the library intent API directly;
//! this binary never shells out to `O`, `olangc`, `o-link`, or `o-node`.

use anyhow::{bail, Context, Result};
#[cfg(test)]
use clap::CommandFactory;
use clap::{Args, Parser, Subcommand, ValueEnum};
use o_lang::boot_objects::{
    portable_boot_object_ref, BootObjectIndex, BootObjectRecord, BootObjectStore, BootPathBinding,
    BOOT_OBJECT_STORE_ENV, DEFAULT_BOOT_OBJECT_STORE,
};
use o_lang::computation::OComputationBuilderV1;
use o_lang::computation_core::{
    artifact_id_for_bytes, ComputationLineageId, ComputationTokenV1, DerivationInputV1,
    DerivationRefV1, DerivationRelationV1, FacetIdV1, FacetKindV1, FacetRefV1, TransformIdentityV1,
};
use o_lang::evidence::{
    source_sha256, ExecutionIntentV1, ADMISSION_SCHEMA_V6, SCHEDULE_EXPLANATION_SCHEMA_V2,
    SCHEDULE_PREDICTION_SCHEMA_V1, SCHEDULE_REALIZABILITY_SCHEMA_V1,
};
use o_lang::hosted_remote::project_mesh::{
    MeshExecutionConfig, MeshExecutionError, MeshExecutionFailureClass, MeshLocalFallback,
    MeshRequirement, MeshTraceEventV1,
};
use o_lang::intent::{
    decoded_value_result_references, execute_prepared_intent, explain_verified_run,
    live_placement_preview, parse_run_selector, prepare_execution_intent,
    render_ordinary_value_stdout_with_color, route_result_references, CapturedStreamV1,
    ExecutionObservationV1, LocalOExecutorV1, OrdinaryExecutionTraceV1, OrdinaryOExecutionErrorV1,
    PrepareExecutionOptionsV1, PreparedExecutionIntentV1, ProjectExecutorV1, RecordedRouteResultV1,
    RunDispositionV1, RunFailureV1, RunRecordV1, RunRecordingStatusV1, RunResultReferenceV1,
    RunStoreReaderV1, RunStoreV1, RunSummaryV1, RunTraceAttachmentV1, RunTraceBindingV1,
    RUN_SUMMARY_SCHEMA_V1,
};
use o_lang::project::executor::{ProjectExecutionError, ProjectExecutionFailureClass};
use o_lang::project::model::OutputCapture;
use o_lang::project::runtime::public_route_execution_diagnostic;
use o_lang::project::{OExecutionResult, RoutePolicy, ValidatedSelectionReceiptV1};
use o_lang::resource_identity::ArtifactId;
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OPERATIONAL_COMMANDS: &str = "Run highlights:\n  o run FILE.O --parallel auto          local HGraph workers only\n  o run PROJECT --parallel auto         mesh prefer with safe local fallback\n  o run PROJECT --mesh=required         authenticated remote placement required\n  Mesh controls include --mesh-retries, --mesh-local-fallback, and --closed-registry.\n\nBoot-object commands:\n  o object root|list|stat|get|verify     typed read-only boot CAS\n\nOperational commands retained by the repository dispatcher:\n  node start|stop|status|restart|pair|list|use|profile|doctor|run|session ...\n  node-host <command> ...\n  registry <command> ...\n  info <command> ...\n  live <command> ...\n  receipt [ogit arguments]\n  kernel <command>\n  why FILE.O P<N> [olangc options]\n\nUnknown command forms retain historical evaluator behavior.";
#[derive(Debug, Parser)]
#[command(
    name = "o",
    version,
    about = "One intent-oriented front door for Ostadix execution and evidence",
    long_about = "Run or plan an Ostadix document or source-closed heterogeneous project, then explain or inspect retained execution evidence.",
    after_long_help = OPERATIONAL_COMMANDS,
    disable_help_subcommand = false,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: IntentCommand,
}

#[derive(Debug, Subcommand)]
enum IntentCommand {
    /// Run a local .O document or a route-preserving heterogeneous project.
    Run(RunArgs),
    /// Build a non-executing static plan, or opt into a read-only live snapshot.
    Plan(PlanArgs),
    /// Explain the placement and execution decisions retained for a run.
    Explain(ExplainArgs),
    /// Inspect retained execution evidence and, optionally, its event trace.
    Inspect(InspectArgs),
    /// Bind exact semantic-custody artifacts into one canonical computation record.
    Computation(ComputationArgs),
    /// Inspect and read the typed, authority-free boot-object store.
    Object(ObjectArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ParallelMode {
    /// Choose useful local/project parallelism and existing eligible peers.
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ExecutorMode {
    Serial,
    Graph,
}

impl ExecutorMode {
    const fn intent(self) -> LocalOExecutorV1 {
        match self {
            Self::Serial => LocalOExecutorV1::ForcedSerial,
            Self::Graph => LocalOExecutorV1::ForcedGraph,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MeshMode {
    Prefer,
    Required,
}

impl MeshMode {
    const fn requirement(self) -> MeshRequirement {
        match self {
            Self::Prefer => MeshRequirement::Prefer,
            Self::Required => MeshRequirement::Required,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MeshFallbackMode {
    PreSend,
    Idempotent,
    Never,
}

impl MeshFallbackMode {
    const fn intent(self) -> MeshLocalFallback {
        match self {
            Self::PreSend => MeshLocalFallback::PreSend,
            Self::Idempotent => MeshLocalFallback::Idempotent,
            Self::Never => MeshLocalFallback::Never,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PlanFormat {
    Text,
    Json,
}

#[derive(Clone, Debug, Args)]
struct RunArgs {
    /// A .O source/lifted bundle or project directory.
    #[arg(value_name = "TARGET")]
    target: PathBuf,

    /// Historical positional shim directory accepted for ordinary .O runs.
    #[arg(value_name = "BACKENDS")]
    legacy_backends: Option<PathBuf>,

    /// Automatically use useful local parallelism and already-running peers.
    #[arg(long, value_enum)]
    parallel: Option<ParallelMode>,

    /// Produce one versioned run-summary JSON envelope on stdout.
    #[arg(long)]
    json: bool,

    /// Do not retain this invocation as a run record.
    #[arg(long, conflicts_with = "require_record")]
    no_record: bool,

    /// Fail the command if its run record cannot be durably finalized.
    #[arg(long, conflicts_with = "no_record")]
    require_record: bool,

    /// Explicitly interpret TARGET as a route-preserving project input.
    #[arg(long)]
    project: bool,

    /// Shim directory used by local execution.
    #[arg(long = "shim-dir")]
    shim_dir: Option<PathBuf>,

    /// Install one compatibility backend grant (repeatable).
    #[arg(long = "backend-grant", value_name = "NAME=LANG[:RIGHT,...]")]
    backend_grants: Vec<String>,

    /// Select the ordinary .O execution engine.
    #[arg(long, value_enum)]
    executor: Option<ExecutorMode>,

    /// Bound the ordinary local HGraph worker pool.
    #[arg(long, value_parser = parse_positive_usize)]
    workers: Option<usize>,

    /// Select one project route or route set.
    #[arg(long)]
    route: Option<String>,

    /// Override the selected project route policy.
    #[arg(long = "routes-policy", visible_alias = "route-policy")]
    routes_policy: Option<String>,

    /// Add or replace a project route declaration (repeatable).
    #[arg(long = "route-decl")]
    route_decls: Vec<String>,

    /// Write the local Project HGraph attempt trace to an explicit path.
    #[arg(long = "project-trace-out")]
    project_trace_out: Option<PathBuf>,

    /// Write the unsigned validated benchmark-selection receipt as JSON.
    #[arg(long = "selection-receipt-out")]
    selection_receipt_out: Option<PathBuf>,

    /// Enable explicit peer-mesh execution. Bare --mesh means prefer.
    #[arg(
        long,
        value_enum,
        num_args = 0..=1,
        default_missing_value = "prefer",
        require_equals = true
    )]
    mesh: Option<MeshMode>,

    /// Additional remote actor generations after the first attempt.
    #[arg(long = "mesh-retries", value_parser = parse_mesh_retries)]
    mesh_retries: Option<u32>,

    /// Local fallback rule after mesh placement was requested.
    #[arg(long = "mesh-local-fallback", value_enum)]
    mesh_local_fallback: Option<MeshFallbackMode>,

    /// Maximum live peer-discovery interval.
    #[arg(
        long = "mesh-discovery-timeout-ms",
        value_parser = parse_discovery_timeout_ms
    )]
    mesh_discovery_timeout_ms: Option<u64>,

    /// Use only the selected paired-peer registry.
    #[arg(long = "mesh-no-lan-discovery", visible_alias = "closed-registry")]
    mesh_no_lan_discovery: bool,

    /// Override the paired-peer registry root.
    #[arg(long = "mesh-peer-root")]
    mesh_peer_root: Option<PathBuf>,

    /// Write the mesh placement/retry/fallback trace to an explicit path.
    #[arg(long = "mesh-trace-out")]
    mesh_trace_out: Option<PathBuf>,

    /// Explain live mesh candidate and placement decisions on stderr.
    #[arg(long = "explain-mesh")]
    explain_mesh: bool,
}

#[derive(Debug, Args)]
struct PlanArgs {
    /// A .O source/lifted bundle or project directory.
    #[arg(value_name = "TARGET")]
    target: PathBuf,

    /// Analyze useful local/project parallelism without dispatching it.
    #[arg(long, value_enum)]
    parallel: Option<ParallelMode>,

    /// Add read-only discovery/profile/capacity observations to the static plan.
    #[arg(long, requires = "parallel")]
    live: bool,

    /// Select one project route or route set.
    #[arg(long)]
    route: Option<String>,

    /// Override the selected project route policy.
    #[arg(
        long = "routes-policy",
        visible_alias = "route-policy",
        requires = "route"
    )]
    routes_policy: Option<String>,

    /// Add or replace a project route declaration (repeatable).
    #[arg(long = "route-decl")]
    route_decls: Vec<String>,

    /// Shim directory used for local runtime inspection.
    #[arg(long = "shim-dir")]
    shim_dir: Option<PathBuf>,

    /// Install one compatibility backend grant in the inspected context.
    #[arg(long = "backend-grant")]
    backend_grants: Vec<String>,

    /// Append the ordinary .O static admission and schedule explanation.
    #[arg(long = "explain-schedule")]
    explain_schedule: bool,

    /// Select text or strict JSON schedule rendering.
    #[arg(long, value_enum)]
    format: Option<PlanFormat>,

    /// Emit the versioned static/live intent-plan envelope as JSON.
    #[arg(long, conflicts_with = "format")]
    json: bool,

    /// Override the local-worker count in the inspection-only view.
    #[arg(long, requires = "live", value_parser = parse_positive_usize)]
    workers: Option<usize>,

    /// Emit the authority-free execution-intent identity for ordinary .O.
    #[arg(long = "execution-intent-json")]
    execution_intent_json: bool,

    /// Append the governed/ambient grounding report.
    #[arg(long)]
    grounding: bool,

    #[arg(long = "world-id")]
    world_id: Option<String>,

    #[arg(long = "world-epoch")]
    world_epoch: Option<u64>,

    /// Live-plan peer discovery interval.
    #[arg(
        long = "mesh-discovery-timeout-ms",
        requires = "live",
        value_parser = parse_discovery_timeout_ms
    )]
    mesh_discovery_timeout_ms: Option<u64>,

    /// In live mode, inspect only the selected paired-peer registry.
    #[arg(
        long = "mesh-no-lan-discovery",
        visible_alias = "closed-registry",
        requires = "live"
    )]
    mesh_no_lan_discovery: bool,

    /// Override the paired-peer registry used by live planning.
    #[arg(long = "mesh-peer-root", requires = "live")]
    mesh_peer_root: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ExplainArgs {
    /// `last-run` or a retained run identifier.
    #[arg(value_name = "RUN", default_value = "last-run")]
    run: String,

    /// Emit a versioned machine-readable explanation.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct InspectArgs {
    /// `last-run` or a retained run identifier.
    #[arg(value_name = "RUN", default_value = "last-run")]
    run: String,

    /// Include the full bounded event trace.
    #[arg(long)]
    trace: bool,

    /// Emit the retained record as versioned JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ObjectArgs {
    /// Override the immutable boot-object store root.
    #[arg(long, global = true, value_name = "DIR")]
    store: Option<PathBuf>,

    #[command(subcommand)]
    command: ObjectCommand,
}

#[derive(Debug, Subcommand)]
enum ObjectCommand {
    /// Print the source identities, set root, counts, and byte totals.
    Root(ObjectRootArgs),
    /// List canonical path bindings.
    List(ObjectListArgs),
    /// Describe one path, raw SHA-256, or Git blob SHA-1 selector.
    Stat(ObjectStatArgs),
    /// Read one fully verified blob without executing it.
    Get(ObjectGetArgs),
    /// Fully verify the canonical index and exact CAS object closure.
    Verify(ObjectVerifyArgs),
}

#[derive(Debug, Args)]
struct ObjectRootArgs {
    /// Emit one stable JSON object.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ObjectListArgs {
    /// Restrict results to this exact path or its descendants.
    #[arg(long, value_name = "PATH")]
    prefix: Option<String>,

    /// Emit one stable JSON array.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ObjectStatArgs {
    /// PATH, sha256:<64 lowercase hex>, or git-sha1:<40 lowercase hex>.
    #[arg(value_name = "SELECTOR")]
    selector: String,

    /// Emit one stable JSON object.
    #[arg(long, conflicts_with = "owvalue")]
    json: bool,

    /// Emit the canonical binary OWVALUE boot-object reference.
    #[arg(long, conflicts_with = "json")]
    owvalue: bool,
}

#[derive(Debug, Args)]
struct ObjectGetArgs {
    /// PATH, sha256:<64 lowercase hex>, or git-sha1:<40 lowercase hex>.
    #[arg(value_name = "SELECTOR")]
    selector: String,

    /// New output path, or `-` for stdout. Existing paths are never overwritten.
    #[arg(short, long, value_name = "FILE", default_value = "-")]
    output: PathBuf,

    /// Permit raw bytes on an interactive terminal.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct ObjectVerifyArgs {
    /// Emit one stable JSON object instead of the boot smoke marker.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ComputationArgs {
    /// Exact .O source bytes described by the execution intent.
    #[arg(long, value_name = "PATH")]
    source: PathBuf,

    /// Stable execution-intent JSON emitted for the source.
    #[arg(long, value_name = "PATH")]
    execution_intent: PathBuf,

    /// Inspection-only schedule explanation text emitted for the source.
    #[arg(long, value_name = "PATH")]
    schedule: PathBuf,

    /// Graphviz rendering of the solved HGraph.
    #[arg(long, value_name = "PATH")]
    hgraph_dot: PathBuf,

    /// Observed JSON result of the same-intent-gated execution.
    #[arg(long, value_name = "PATH")]
    result: PathBuf,

    /// Exact O runtime executable used for the observed result.
    #[arg(long, value_name = "PATH")]
    o_bin: PathBuf,

    /// Exact olangc executable used for the inspection artifacts.
    #[arg(long, value_name = "PATH")]
    olangc_bin: PathBuf,

    /// Refuse-overwrite output path for the canonical CBOR body.
    #[arg(long, value_name = "PATH")]
    cbor_out: PathBuf,

    /// Refuse-overwrite output path for the matching JSON projection.
    #[arg(long, value_name = "PATH")]
    json_out: PathBuf,

    /// Enduring computation lineage named by this revision.
    #[arg(long, default_value = "examples/semantic-custody")]
    lineage: String,
}

fn main() {
    // Hosted backend workers relaunch the exact admitted current executable.
    // The unified front door therefore owns the same hidden backend protocol
    // entrypoint as `O`; ordinary user arguments still flow through Clap.
    match o_lang::backend::run_backend_from_env_args() {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::exit(1);
        }
    }
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let arguments = env::args_os().collect::<Vec<_>>();
            let informational = matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            );
            if !informational && invocation_requests_run_json(&arguments) {
                let detail = error.to_string();
                if let Err(summary_error) = emit_preflight_failure_summary(&detail) {
                    eprintln!("error: failed to encode preflight run summary: {summary_error:#}");
                }
                eprint!("{error}");
                std::process::exit(error.exit_code());
            }
            error.exit();
        }
    };
    match dispatch(cli.command) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::exit(1);
        }
    }
}

fn invocation_requests_run_json(arguments: &[OsString]) -> bool {
    arguments.get(1).is_some_and(|value| value == "run")
        && arguments.iter().skip(2).any(|value| value == "--json")
}

fn emit_preflight_failure_summary(detail: &str) -> Result<()> {
    let summary = RunSummaryV1::preflight_failed(detail);
    summary
        .validate()
        .map_err(anyhow::Error::msg)
        .context("front door produced an invalid preflight-failure run summary")?;
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}

fn dispatch(command: IntentCommand) -> Result<i32> {
    match command {
        IntentCommand::Run(args) => run_intent(&args),
        IntentCommand::Plan(args) => plan_intent(&args),
        IntentCommand::Explain(args) => explain_pending(&args),
        IntentCommand::Inspect(args) => inspect_pending(&args),
        IntentCommand::Computation(args) => computation_artifact(&args),
        IntentCommand::Object(args) => object_command(&args),
    }
}

fn computation_artifact(args: &ComputationArgs) -> Result<i32> {
    if args.cbor_out == args.json_out {
        bail!("canonical CBOR and JSON outputs must use distinct paths");
    }

    let source = read_computation_input(&args.source, "source")?;
    let intent_bytes = read_computation_input(&args.execution_intent, "execution intent")?;
    let schedule_bytes = read_computation_input(&args.schedule, "schedule explanation")?;
    let hgraph_dot = read_computation_input(&args.hgraph_dot, "HGraph DOT rendering")?;
    let result_bytes = read_computation_input(&args.result, "observed result")?;

    let intent: ExecutionIntentV1 = serde_json::from_slice(&intent_bytes).with_context(|| {
        format!(
            "invalid execution-intent JSON {}",
            args.execution_intent.display()
        )
    })?;
    intent
        .validate()
        .context("execution-intent JSON failed canonical semantic validation")?;
    let actual_source_sha256 = source_sha256(&source);
    if intent.source_sha256 != actual_source_sha256 {
        bail!(
            "execution intent names source SHA-256 {}, but {} hashes to {}",
            intent.source_sha256,
            args.source.display(),
            actual_source_sha256
        );
    }

    validate_schedule_explanation(&schedule_bytes, &intent, &args.schedule)?;
    validate_hgraph_rendering(&hgraph_dot, &args.hgraph_dot)?;
    validate_observed_result(&result_bytes, &args.result)?;

    let o_identity = executable_identity(&args.o_bin, "O runtime")?;
    let olangc_identity = executable_identity(&args.olangc_bin, "olangc compiler")?;

    let source_id = computation_id("source")?;
    let o_binary_id = computation_id("tool/o-binary")?;
    let olangc_binary_id = computation_id("tool/olangc-binary")?;
    let intent_id = computation_id("execution-intent")?;
    let schedule_id = computation_id("schedule-explanation")?;
    let hgraph_id = computation_id("hgraph-rendering")?;
    let result_id = computation_id("terminal-observation")?;

    let native_executable_schema = computation_token("ostadix/native-executable/unversioned")?;
    let mut builder = OComputationBuilderV1::new(ComputationLineageId::new(args.lineage.clone())?);
    builder
        .add_root_facet(FacetRefV1::new(
            source_id.clone(),
            FacetKindV1::Source,
            computation_token("ostadix.source/o/v1")?,
            artifact_id_for_bytes(&source),
        ))
        .add_root_facet(FacetRefV1::new(
            o_binary_id.clone(),
            FacetKindV1::NativePackage,
            native_executable_schema.clone(),
            o_identity.clone(),
        ))
        .add_root_facet(FacetRefV1::new(
            olangc_binary_id.clone(),
            FacetKindV1::NativePackage,
            native_executable_schema,
            olangc_identity.clone(),
        ))
        .add_facet_bytes(
            intent_id.clone(),
            FacetKindV1::ExecutionIntent,
            computation_token(&intent.schema)?,
            &intent_bytes,
        )
        .add_facet_bytes(
            schedule_id.clone(),
            FacetKindV1::ScheduleExplanation,
            computation_token(SCHEDULE_EXPLANATION_SCHEMA_V2)?,
            &schedule_bytes,
        )
        .add_facet_bytes(
            hgraph_id.clone(),
            FacetKindV1::HgraphRendering,
            computation_token("ostadix.hgraph-rendering/dot-v1")?,
            &hgraph_dot,
        )
        .add_facet_bytes(
            result_id.clone(),
            FacetKindV1::TerminalObservation,
            computation_token("ostadix.o-json-result/unversioned")?,
            &result_bytes,
        )
        .add_derivation(DerivationRefV1::new(
            DerivationRelationV1::AnalyzedFrom,
            vec![
                computation_input("source", source_id.clone())?,
                computation_input("compiler_binary", olangc_binary_id.clone())?,
            ],
            intent_id.clone(),
            exact_transform(
                "ostadix/workflow-attested/olangc-execution-intent-json/v1",
                &olangc_identity,
            )?,
        ))
        .add_derivation(DerivationRefV1::new(
            DerivationRelationV1::AnalyzedFrom,
            vec![
                computation_input("source", source_id.clone())?,
                computation_input("compiler_binary", olangc_binary_id.clone())?,
            ],
            schedule_id,
            exact_transform(
                "ostadix/workflow-attested/olangc-schedule-explanation/v2",
                &olangc_identity,
            )?,
        ))
        .add_derivation(DerivationRefV1::new(
            DerivationRelationV1::ProjectedFrom,
            vec![
                computation_input("source", source_id.clone())?,
                computation_input("compiler_binary", olangc_binary_id)?,
            ],
            hgraph_id,
            exact_transform(
                "ostadix/workflow-attested/olangc-hgraph-dot/v1",
                &olangc_identity,
            )?,
        ))
        .add_derivation(DerivationRefV1::new(
            DerivationRelationV1::ObservedFrom,
            vec![
                computation_input("source", source_id)?,
                computation_input("required_execution_intent", intent_id)?,
                computation_input("runtime_binary", o_binary_id)?,
            ],
            result_id,
            exact_transform(
                "ostadix/workflow-attested/o-same-intent-graph-execution/v1",
                &o_identity,
            )?,
        ));

    let computation = builder
        .finish()
        .context("semantic-custody facets do not form a valid computation")?;
    let canonical_cbor = computation
        .canonical_bytes()
        .context("failed to encode canonical computation CBOR")?;
    let canonical_json = computation
        .canonical_json_pretty()
        .context("failed to encode computation JSON projection")?;
    write_new_private(
        &args.cbor_out,
        &canonical_cbor,
        "canonical computation CBOR",
    )?;
    write_new_private(
        &args.json_out,
        &canonical_json,
        "computation JSON projection",
    )?;
    println!("{}", computation.revision().as_sha256());
    Ok(0)
}

fn read_computation_input(path: &Path, label: &str) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("failed to read {label} {}", path.display()))
}

fn validate_schedule_explanation(
    bytes: &[u8],
    intent: &ExecutionIntentV1,
    path: &Path,
) -> Result<()> {
    let text = std::str::from_utf8(bytes)
        .with_context(|| format!("schedule explanation {} is not UTF-8", path.display()))?;
    // The V6 schedule's analyzed-graph digest is evidence-domain identity, not
    // ExecutionIntentV1's stable analyzed-graph digest. OIR, plan, and catalog
    // projection are the coordinates the two inspection views actually share.
    for required in [
        format!("; ExecutionAdmission {ADMISSION_SCHEMA_V6}"),
        format!("lowered-oir-sha256={}", intent.oir_sha256),
        format!("plan-sha256={}", intent.plan_sha256),
        format!(
            "backend-catalog-projection-sha256={}",
            intent.backend_catalog_projection_sha256
        ),
        "runtime-snapshot kind=inspection dispatch-context=inspection-only".to_string(),
        format!("; ScheduleRealizability {SCHEDULE_REALIZABILITY_SCHEMA_V1}"),
        "execution-realizable=unknown dispatch=not-run".to_string(),
        format!("; SchedulePrediction {SCHEDULE_PREDICTION_SCHEMA_V1}"),
    ] {
        if !text.contains(&required) {
            bail!(
                "schedule explanation {} is missing required binding `{required}`",
                path.display()
            );
        }
    }
    Ok(())
}

fn validate_hgraph_rendering(bytes: &[u8], path: &Path) -> Result<()> {
    let text = std::str::from_utf8(bytes)
        .with_context(|| format!("HGraph DOT rendering {} is not UTF-8", path.display()))?;
    let rendered = text.trim();
    if !rendered.starts_with("digraph hgraph {") || !rendered.ends_with('}') {
        bail!(
            "HGraph DOT rendering {} is not a complete `digraph hgraph` view",
            path.display()
        );
    }
    Ok(())
}

fn validate_observed_result(bytes: &[u8], path: &Path) -> Result<()> {
    let result: serde_json::Value = serde_json::from_slice(bytes)
        .with_context(|| format!("observed result {} is not JSON", path.display()))?;
    let object = result
        .as_object()
        .with_context(|| format!("observed result {} is not a JSON object", path.display()))?;
    if object.get("ok") != Some(&serde_json::Value::Bool(true)) {
        bail!("observed result {} is not successful", path.display());
    }
    if !object
        .get("value")
        .is_some_and(serde_json::Value::is_object)
    {
        bail!(
            "observed result {} has no structured O value",
            path.display()
        );
    }
    if !object.get("type").is_some_and(serde_json::Value::is_string) {
        bail!("observed result {} has no value type", path.display());
    }
    if !object
        .get("elapsed_ms")
        .is_some_and(serde_json::Value::is_u64)
    {
        bail!(
            "observed result {} has no elapsed_ms observation",
            path.display()
        );
    }
    Ok(())
}

fn executable_identity(path: &Path, label: &str) -> Result<ArtifactId> {
    let mut executable =
        File::open(path).with_context(|| format!("failed to open {label} {}", path.display()))?;
    let metadata = executable
        .metadata()
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        bail!("{label} {} is not a non-empty regular file", path.display());
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o111 == 0 {
        bail!("{label} {} is not executable", path.display());
    }

    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = executable
            .read(&mut buffer)
            .with_context(|| format!("failed to hash {label} {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(ArtifactId::from_sha256(hex::encode(hasher.finalize()))?)
}

fn computation_id(value: &str) -> Result<FacetIdV1> {
    Ok(FacetIdV1::new(value)?)
}

fn computation_token(value: &str) -> Result<ComputationTokenV1> {
    Ok(ComputationTokenV1::new(value)?)
}

fn computation_input(role: &str, facet: FacetIdV1) -> Result<DerivationInputV1> {
    Ok(DerivationInputV1::new(computation_token(role)?, facet))
}

fn exact_transform(name: &str, implementation: &ArtifactId) -> Result<TransformIdentityV1> {
    Ok(TransformIdentityV1::new(
        computation_token(name)?,
        implementation.clone(),
    ))
}

fn write_new_private(path: &Path, bytes: &[u8], label: &str) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut output = options
        .open(path)
        .with_context(|| format!("refusing to overwrite {label} {}", path.display()))?;
    output
        .write_all(bytes)
        .with_context(|| format!("failed to write {label} {}", path.display()))?;
    output
        .sync_all()
        .with_context(|| format!("failed to synchronize {label} {}", path.display()))?;
    Ok(())
}

struct ObjectSelection<'a> {
    object: &'a BootObjectRecord,
    selected_binding: Option<&'a BootPathBinding>,
}

fn object_command(args: &ObjectArgs) -> Result<i32> {
    let root = args
        .store
        .clone()
        .or_else(|| env::var_os(BOOT_OBJECT_STORE_ENV).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_BOOT_OBJECT_STORE));
    let store = BootObjectStore::open(&root)
        .with_context(|| format!("failed to open boot-object store {}", root.display()))?;
    match &args.command {
        ObjectCommand::Root(command) => object_root(store.index(), command),
        ObjectCommand::List(command) => object_list(store.index(), command),
        ObjectCommand::Stat(command) => object_stat(store.index(), command),
        ObjectCommand::Get(command) => object_get(&store, command),
        ObjectCommand::Verify(command) => object_verify(&store, command),
    }
}

fn object_root(index: &BootObjectIndex, args: &ObjectRootArgs) -> Result<i32> {
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": "ostadix.boot-object-set/v1",
                "source_commit_sha1": hex::encode(index.source_commit()),
                "source_tree_sha1": hex::encode(index.source_tree()),
                "root_sha256": hex::encode(index.root_sha256()),
                "objects": index.objects().len(),
                "bindings": index.bindings().len(),
                "logical_bytes": index.logical_bytes(),
                "stored_bytes": index.stored_bytes(),
            }))?
        );
    } else {
        println!("schema=ostadix.boot-object-set/v1");
        println!("source_commit_sha1={}", hex::encode(index.source_commit()));
        println!("source_tree_sha1={}", hex::encode(index.source_tree()));
        println!("root_sha256={}", hex::encode(index.root_sha256()));
        println!("objects={}", index.objects().len());
        println!("bindings={}", index.bindings().len());
        println!("logical_bytes={}", index.logical_bytes());
        println!("stored_bytes={}", index.stored_bytes());
    }
    Ok(0)
}

fn object_list(index: &BootObjectIndex, args: &ObjectListArgs) -> Result<i32> {
    let bindings = index
        .bindings()
        .iter()
        .filter(|binding| {
            args.prefix
                .as_ref()
                .is_none_or(|prefix| path_matches_prefix(binding.path(), prefix))
        })
        .collect::<Vec<_>>();
    if args.json {
        let values = bindings
            .iter()
            .map(|binding| {
                serde_json::json!({
                    "path": binding.path(),
                    "mode": binding.mode().as_octal(),
                    "executable": binding.mode().is_executable(),
                    "sha256": hex::encode(binding.object_sha256()),
                })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&values)?);
    } else {
        for binding in bindings {
            println!(
                "{}\t{}\tsha256:{}",
                binding.mode().as_octal(),
                binding.path(),
                hex::encode(binding.object_sha256())
            );
        }
    }
    Ok(0)
}

fn object_stat(index: &BootObjectIndex, args: &ObjectStatArgs) -> Result<i32> {
    let selection = resolve_object(index, &args.selector)?;
    if args.owvalue {
        let record = portable_boot_object_ref(*index.root_sha256(), selection.object)?;
        let encoded = record.encode()?;
        let mut output = io::stdout().lock();
        output.write_all(&encoded)?;
        output.flush()?;
        return Ok(0);
    }

    let paths = index
        .bindings_for_object(selection.object.sha256())
        .map(|binding| {
            serde_json::json!({
                "path": binding.path(),
                "mode": binding.mode().as_octal(),
                "executable": binding.mode().is_executable(),
            })
        })
        .collect::<Vec<_>>();
    let identity = selection.object.identity()?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": "ostadix.boot-object-ref/v1",
                "identity": {
                    "world": identity.world().as_str(),
                    "object": identity.object().as_str(),
                    "version": identity.version().get(),
                },
                "kind": "git-blob",
                "sha256": selection.object.sha256_hex(),
                "git_sha1": selection.object.git_sha1_hex(),
                "bytes": selection.object.bytes(),
                "set_sha256": hex::encode(index.root_sha256()),
                "selected_path": selection.selected_binding.map(BootPathBinding::path),
                "paths": paths,
            }))?
        );
    } else {
        println!("schema=ostadix.boot-object-ref/v1");
        println!("identity={identity}");
        println!("kind=git-blob");
        println!("sha256={}", selection.object.sha256_hex());
        println!("git_sha1={}", selection.object.git_sha1_hex());
        println!("bytes={}", selection.object.bytes());
        println!("set_sha256={}", hex::encode(index.root_sha256()));
        if let Some(binding) = selection.selected_binding {
            println!("selected_path={}", binding.path());
        }
        for path in paths {
            println!(
                "path={}\tmode={}",
                path["path"].as_str().expect("path projection is text"),
                path["mode"].as_str().expect("mode projection is text")
            );
        }
    }
    Ok(0)
}

fn object_get(store: &BootObjectStore, args: &ObjectGetArgs) -> Result<i32> {
    let selection = resolve_object(store.index(), &args.selector)?;
    let bytes = store
        .read_object(selection.object.sha256())
        .with_context(|| format!("failed to verify boot object `{}`", args.selector))?;
    if args.output.as_os_str() == "-" {
        if io::stdout().is_terminal() && !args.force {
            bail!("refusing to write raw boot-object bytes to a terminal; use --force or --output FILE");
        }
        let mut output = io::stdout().lock();
        output.write_all(&bytes)?;
        output.flush()?;
        return Ok(0);
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut output = options
        .open(&args.output)
        .with_context(|| format!("refusing to overwrite output {}", args.output.display()))?;
    output
        .write_all(&bytes)
        .with_context(|| format!("failed to write {}", args.output.display()))?;
    output
        .sync_all()
        .with_context(|| format!("failed to synchronize {}", args.output.display()))?;
    Ok(0)
}

fn object_verify(store: &BootObjectStore, args: &ObjectVerifyArgs) -> Result<i32> {
    let report = store
        .verify()
        .context("full boot-object index/CAS verification failed")?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": "ostadix.boot-object-verification/v1",
                "status": "pass",
                "root_sha256": hex::encode(report.root_sha256),
                "objects": report.object_count,
                "bindings": report.binding_count,
                "logical_bytes": report.logical_bytes,
                "stored_bytes": report.stored_bytes,
            }))?
        );
    } else {
        println!("OSTADIX BOOT OBJECTS: PASS");
        println!("root_sha256={}", hex::encode(report.root_sha256));
        println!("objects={}", report.object_count);
        println!("bindings={}", report.binding_count);
        println!("logical_bytes={}", report.logical_bytes);
        println!("stored_bytes={}", report.stored_bytes);
    }
    Ok(0)
}

fn resolve_object<'a>(index: &'a BootObjectIndex, selector: &str) -> Result<ObjectSelection<'a>> {
    if let Some(value) = selector.strip_prefix("sha256:") {
        let digest = decode_lower_hex::<32>(value, "SHA-256")?;
        let object = index
            .object_by_sha256(&digest)
            .ok_or_else(|| anyhow::anyhow!("boot object `{selector}` is absent from the index"))?;
        return Ok(ObjectSelection {
            object,
            selected_binding: None,
        });
    }
    if let Some(value) = selector.strip_prefix("git-sha1:") {
        let oid = decode_lower_hex::<20>(value, "Git SHA-1")?;
        let object = index
            .object_by_git_sha1(&oid)
            .ok_or_else(|| anyhow::anyhow!("Git blob `{selector}` is absent from the index"))?;
        return Ok(ObjectSelection {
            object,
            selected_binding: None,
        });
    }
    let binding = index
        .binding_by_path(selector)
        .ok_or_else(|| anyhow::anyhow!("boot-object path `{selector}` is absent from the index"))?;
    let object = index
        .object_by_sha256(binding.object_sha256())
        .ok_or_else(|| anyhow::anyhow!("boot-object index contains a dangling path binding"))?;
    Ok(ObjectSelection {
        object,
        selected_binding: Some(binding),
    })
}

fn decode_lower_hex<const N: usize>(value: &str, label: &str) -> Result<[u8; N]> {
    if value.len() != N * 2
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!(
            "{label} must be exactly {} lowercase hexadecimal characters",
            N * 2
        );
    }
    let decoded = hex::decode(value).expect("validated hexadecimal input");
    decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("{label} decoded to the wrong length"))
}

fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    prefix.is_empty()
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|remainder| remainder.starts_with('/'))
}

fn resolve_shim_dir(explicit: Option<&Path>, positional: Option<&Path>) -> Result<PathBuf> {
    if explicit.is_some() && positional.is_some() {
        bail!("specify the shim directory either positionally or with --shim-dir, not both");
    }
    Ok(explicit
        .or(positional)
        .map(Path::to_path_buf)
        .or_else(|| env::var_os("O_BACKENDS_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends")))
}

fn checked_route_policy(value: Option<&str>) -> Result<Option<RoutePolicy>> {
    value
        .map(RoutePolicy::parse_checked)
        .transpose()
        .map_err(anyhow::Error::msg)
}

fn run_has_mesh_tuning(args: &RunArgs) -> bool {
    args.mesh_retries.is_some()
        || args.mesh_local_fallback.is_some()
        || args.mesh_discovery_timeout_ms.is_some()
        || args.mesh_no_lan_discovery
        || args.mesh_peer_root.is_some()
        || args.mesh_trace_out.is_some()
        || args.explain_mesh
}

fn run_mesh_config(args: &RunArgs) -> Result<Option<MeshExecutionConfig>> {
    let has_tuning = run_has_mesh_tuning(args);
    let enabled = args.mesh.is_some() || has_tuning;
    if args.parallel != Some(ParallelMode::Auto) && args.mesh.is_none() && has_tuning {
        bail!("mesh tuning options require --parallel auto or --mesh[=prefer|required]");
    }
    if !enabled {
        return Ok(None);
    }
    let mut config = MeshExecutionConfig::default();
    if let Some(mode) = args.mesh {
        config.requirement = mode.requirement();
    }
    if let Some(retries) = args.mesh_retries {
        config.max_retries = retries;
    }
    if let Some(fallback) = args.mesh_local_fallback {
        config.local_fallback = fallback.intent();
    }
    if let Some(timeout) = args.mesh_discovery_timeout_ms {
        config.discovery_timeout = Duration::from_millis(timeout);
    }
    config.discover_lan = !args.mesh_no_lan_discovery;
    config.peer_root = args.mesh_peer_root.clone();
    config.trace_out = None;
    // The library's legacy explanation path writes directly to process
    // stderr, which cannot be retained byte-for-byte. The front door renders
    // the same observation from the returned trace instead.
    config.explain = false;
    Ok(Some(config))
}

fn run_prepare_options(args: &RunArgs) -> Result<PrepareExecutionOptionsV1> {
    Ok(PrepareExecutionOptionsV1 {
        route: args.route.clone(),
        route_policy: checked_route_policy(args.routes_policy.as_deref())?,
        route_declarations: args.route_decls.clone(),
        parallel_auto: args.parallel == Some(ParallelMode::Auto),
        explicit_mesh: args.mesh.is_some(),
        mesh: run_mesh_config(args)?,
        ordinary_executor: args.executor.map(ExecutorMode::intent),
        local_workers: args.workers,
        backend_grants: args.backend_grants.clone(),
        excluded_project_paths: [
            args.project_trace_out.clone(),
            args.mesh_trace_out.clone(),
            args.selection_receipt_out.clone(),
        ]
        .into_iter()
        .flatten()
        .collect(),
        shim_dir: resolve_shim_dir(args.shim_dir.as_deref(), args.legacy_backends.as_deref())?,
    })
}

fn publication_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .context("failed to resolve the current directory")?
            .join(path)
    };
    let file_name = absolute
        .file_name()
        .context("explicit output path must end in a file name")?;
    file_name
        .to_str()
        .context("explicit output path must end in a UTF-8 file name")?;
    let parent = absolute
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("explicit output path has no parent directory")?;
    let parent = parent
        .canonicalize()
        .with_context(|| format!("failed to resolve output directory {}", parent.display()))?;
    Ok(parent.join(file_name))
}

fn resolve_run_output_paths(args: &RunArgs) -> Result<RunArgs> {
    let mut resolved = args.clone();
    resolved.target = args
        .target
        .canonicalize()
        .with_context(|| format!("failed to resolve input {}", args.target.display()))?;
    for path in [
        &mut resolved.project_trace_out,
        &mut resolved.mesh_trace_out,
        &mut resolved.selection_receipt_out,
    ] {
        if let Some(value) = path {
            *value = publication_path(value)?;
        }
    }
    Ok(resolved)
}

fn validate_explicit_output_paths(
    input: &Path,
    outputs: &[(&str, Option<&PathBuf>)],
) -> Result<()> {
    if outputs.iter().all(|(_, path)| path.is_none()) {
        return Ok(());
    }
    let input_canonical = input.to_path_buf();
    let input_is_directory = input_canonical.is_dir();
    let input_publication = (!input_is_directory)
        .then(|| publication_path(input))
        .transpose()?;
    let mut resolved_outputs: Vec<(&str, PathBuf, Option<PathBuf>)> = Vec::new();
    for (name, path) in outputs {
        let Some(path) = path else {
            continue;
        };
        let resolved = publication_path(path)?;
        let referent = path
            .exists()
            .then(|| {
                path.canonicalize()
                    .with_context(|| format!("failed to resolve output path {}", path.display()))
            })
            .transpose()?;
        if let Some((other_name, _, _)) = resolved_outputs.iter().find(|(_, other, target)| {
            other.as_path() == resolved.as_path()
                || referent
                    .as_ref()
                    .is_some_and(|referent| referent == other || target.as_ref() == Some(referent))
                || target.as_ref() == Some(&resolved)
        }) {
            bail!(
                "{other_name} and {name} must not resolve to the same output path ({})",
                resolved.display()
            );
        }
        if input_publication
            .as_ref()
            .is_some_and(|input| input.as_path() == resolved.as_path())
            || resolved == input_canonical
            || referent.as_ref() == Some(&input_canonical)
            || (input_is_directory
                && (resolved.starts_with(&input_canonical)
                    || referent
                        .as_ref()
                        .is_some_and(|target| target.starts_with(&input_canonical))))
        {
            if input_is_directory {
                bail!(
                    "{name} must be outside the project input directory {}; refusing to replace project input at {}",
                    input_canonical.display(),
                    resolved.display()
                );
            }
            bail!(
                "{name} must not replace the input file {}",
                input_canonical.display()
            );
        }
        resolved_outputs.push((name, resolved, referent));
    }
    Ok(())
}

fn prepare_run(args: &RunArgs) -> Result<PreparedExecutionIntentV1> {
    let explicit_outputs = [
        ("--project-trace-out", args.project_trace_out.as_ref()),
        ("--mesh-trace-out", args.mesh_trace_out.as_ref()),
        (
            "--selection-receipt-out",
            args.selection_receipt_out.as_ref(),
        ),
    ];
    validate_explicit_output_paths(&args.target, &explicit_outputs)?;
    let prepared = prepare_execution_intent(&args.target, run_prepare_options(args)?)?;
    if args.project && matches!(prepared, PreparedExecutionIntentV1::OrdinaryO(_)) {
        bail!("--project requires a project directory or lifted project bundle");
    }
    if args.project_trace_out.is_some()
        && matches!(prepared, PreparedExecutionIntentV1::OrdinaryO(_))
    {
        bail!(
            "--project-trace-out requires a project directory or lifted project bundle; ordinary .O uses its retained evaluator trace"
        );
    }
    if args.selection_receipt_out.is_some()
        && matches!(prepared, PreparedExecutionIntentV1::OrdinaryO(_))
    {
        bail!("--selection-receipt-out requires a project using benchmark_validate_and_select");
    }
    if args.legacy_backends.is_some() && matches!(prepared, PreparedExecutionIntentV1::Project(_)) {
        bail!("the historical positional BACKENDS argument is available only for ordinary .O runs; project routes carry their own runtime declarations");
    }
    if args.shim_dir.is_some() && matches!(prepared, PreparedExecutionIntentV1::Project(_)) {
        bail!("--shim-dir is available only for ordinary .O runs; project routes carry their own runtime declarations");
    }
    if let PreparedExecutionIntentV1::Project(project) = &prepared {
        if args.project_trace_out.is_some() && project.executor != ProjectExecutorV1::Hgraph {
            bail!(
                "--project-trace-out requires O_PROJECT_EXECUTOR=hgraph without mesh execution; use --mesh-trace-out for mesh placement and retry evidence"
            );
        }
        if args.selection_receipt_out.is_some()
            && project.effective_policy != "benchmark_validate_and_select"
        {
            bail!(
                "--selection-receipt-out requires effective project policy benchmark_validate_and_select, got {}",
                project.effective_policy
            );
        }
        if args.selection_receipt_out.is_some() && project.executor == ProjectExecutorV1::Hgraph {
            bail!(
                "--selection-receipt-out is unavailable with O_PROJECT_EXECUTOR=hgraph because that executor does not implement benchmark_validate_and_select"
            );
        }
    }
    Ok(prepared)
}

struct StreamObservation {
    retained: Vec<u8>,
    total_observed_bytes: u64,
    hash: Sha256,
}

impl StreamObservation {
    fn new() -> Self {
        Self {
            retained: Vec::new(),
            total_observed_bytes: 0,
            hash: Sha256::new(),
        }
    }

    fn observe(&mut self, bytes: &[u8]) {
        self.hash.update(bytes);
        self.total_observed_bytes = self
            .total_observed_bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        // This is the front door's terminal stream, not a new bounded runtime
        // capture. Preserve it exactly and let the run store's 256 MiB hard
        // limit decide whether the terminal observation can be finalized.
        self.retained.extend_from_slice(bytes);
    }

    fn finish(self) -> CapturedStreamV1 {
        let retained_bytes = u64::try_from(self.retained.len()).unwrap_or(u64::MAX);
        CapturedStreamV1 {
            retained: self.retained,
            capture: OutputCapture {
                total_observed_bytes: self.total_observed_bytes,
                retained_bytes,
                truncated: self.total_observed_bytes > retained_bytes,
                sha256: hex::encode(self.hash.finalize()),
            },
        }
    }
}

#[cfg(unix)]
struct PreparedProcessCapture {
    stdout_stream: File,
    stderr_stream: File,
    saved_stdout: File,
    saved_stderr: File,
}

#[cfg(unix)]
impl PreparedProcessCapture {
    fn prepare() -> Result<Self> {
        Ok(Self {
            stdout_stream: private_unlinked_capture("stdout")?,
            stderr_stream: private_unlinked_capture("stderr")?,
            saved_stdout: duplicate_descriptor(
                libc::STDOUT_FILENO,
                "failed to preserve process stdout for run observation",
            )?,
            saved_stderr: duplicate_descriptor(
                libc::STDERR_FILENO,
                "failed to preserve process stderr for run observation",
            )?,
        })
    }

    fn execute(
        mut self,
        replay_stdout: bool,
        operation: impl FnOnce() -> ExecutionReport,
    ) -> Result<(
        ExecutionReport,
        StreamObservation,
        StreamObservation,
        Option<anyhow::Error>,
    )> {
        io::stdout()
            .flush()
            .context("failed to flush stdout before run capture")?;
        io::stderr()
            .flush()
            .context("failed to flush stderr before run capture")?;
        // SAFETY: all descriptors are open; `dup2` redirects the process-wide
        // descriptor without taking ownership of either descriptor.
        if unsafe { libc::dup2(self.stdout_stream.as_raw_fd(), libc::STDOUT_FILENO) } < 0 {
            return Err(io::Error::last_os_error())
                .context("failed to activate stdout capture before execution");
        }
        if unsafe { libc::dup2(self.stderr_stream.as_raw_fd(), libc::STDERR_FILENO) } < 0 {
            let activation_error = io::Error::last_os_error();
            // SAFETY: saved stdout remains live. Restore it before returning;
            // the operation has not been invoked at this boundary.
            let _ = unsafe { libc::dup2(self.saved_stdout.as_raw_fd(), libc::STDOUT_FILENO) };
            return Err(activation_error)
                .context("failed to activate stderr capture before execution");
        }

        let report = operation();
        let mut observation_errors = Vec::new();
        if let Err(error) = io::stdout().flush() {
            observation_errors.push(format!(
                "failed to flush captured execution stdout: {error}"
            ));
        }
        if let Err(error) = io::stderr().flush() {
            observation_errors.push(format!(
                "failed to flush captured execution stderr: {error}"
            ));
        }
        // SAFETY: both saved descriptors remain live for this method.
        let stdout_restored =
            unsafe { libc::dup2(self.saved_stdout.as_raw_fd(), libc::STDOUT_FILENO) } >= 0;
        if !stdout_restored {
            observation_errors.push(format!(
                "failed to restore stdout after execution: {}",
                io::Error::last_os_error()
            ));
        }
        let stderr_restored =
            unsafe { libc::dup2(self.saved_stderr.as_raw_fd(), libc::STDERR_FILENO) } >= 0;
        if !stderr_restored {
            observation_errors.push(format!(
                "failed to restore stderr after execution: {}",
                io::Error::last_os_error()
            ));
        }

        let stdout = match drain_captured_stream(
            &mut self.stdout_stream,
            CapturedDescriptor::Stdout,
            replay_stdout && stdout_restored,
        ) {
            Ok((observation, replay_error)) => {
                if let Some(error) = replay_error {
                    observation_errors.push(error);
                }
                observation
            }
            Err(error) => {
                observation_errors.push(format!(
                    "failed to drain captured execution stdout: {error:#}"
                ));
                StreamObservation::new()
            }
        };
        let stderr = match drain_captured_stream(
            &mut self.stderr_stream,
            CapturedDescriptor::Stderr,
            stderr_restored,
        ) {
            Ok((observation, replay_error)) => {
                if let Some(error) = replay_error {
                    observation_errors.push(error);
                }
                observation
            }
            Err(error) => {
                observation_errors.push(format!(
                    "failed to drain captured execution stderr: {error:#}"
                ));
                StreamObservation::new()
            }
        };
        let capture_error = (!observation_errors.is_empty())
            .then(|| anyhow::anyhow!(observation_errors.join("; ")));
        Ok((report, stdout, stderr, capture_error))
    }
}

#[cfg(not(unix))]
struct PreparedProcessCapture;

#[cfg(not(unix))]
impl PreparedProcessCapture {
    fn prepare() -> Result<Self> {
        bail!("run observation requires Unix descriptor capture on this build")
    }

    fn execute(
        self,
        _replay_stdout: bool,
        _operation: impl FnOnce() -> ExecutionReport,
    ) -> Result<(
        ExecutionReport,
        StreamObservation,
        StreamObservation,
        Option<anyhow::Error>,
    )> {
        bail!("run observation requires Unix descriptor capture on this build")
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum CapturedDescriptor {
    Stdout,
    Stderr,
}

#[cfg(unix)]
fn duplicate_descriptor(descriptor: libc::c_int, context: &str) -> Result<File> {
    // SAFETY: `dup` returns a fresh owned descriptor on success.
    let saved = unsafe { libc::dup(descriptor) };
    if saved < 0 {
        return Err(io::Error::last_os_error()).context(context.to_string());
    }
    // SAFETY: `saved` is a fresh descriptor returned by `dup` above.
    Ok(unsafe { File::from_raw_fd(saved) })
}

#[cfg(unix)]
fn private_unlinked_capture(label: &str) -> Result<File> {
    let directory = env::temp_dir();
    for _ in 0..32 {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random)
            .with_context(|| format!("failed to allocate a private {label}-capture name"))?;
        let path = directory.join(format!(
            ".ostadix-o-cli-{label}-{}-{}",
            std::process::id(),
            hex::encode(random)
        ));
        let stream = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(stream) => stream,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create private {label} capture in {}",
                        directory.display()
                    )
                })
            }
        };
        if let Err(error) = fs::remove_file(&path) {
            drop(stream);
            return Err(error).with_context(|| {
                format!(
                    "failed to unlink private {label} capture {}",
                    path.display()
                )
            });
        }
        return Ok(stream);
    }
    bail!("failed to allocate a collision-free private {label} capture")
}

#[cfg(unix)]
fn drain_captured_stream(
    stream: &mut File,
    descriptor: CapturedDescriptor,
    replay: bool,
) -> Result<(StreamObservation, Option<String>)> {
    stream
        .seek(SeekFrom::Start(0))
        .context("failed to rewind captured execution stream")?;
    let mut observation = StreamObservation::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut replay_error = None;
    loop {
        let read = stream
            .read(&mut buffer)
            .context("failed to read captured execution stream")?;
        if read == 0 {
            break;
        }
        let bytes = &buffer[..read];
        observation.observe(bytes);
        if replay && replay_error.is_none() {
            let result = match descriptor {
                CapturedDescriptor::Stdout => io::stdout().write_all(bytes),
                CapturedDescriptor::Stderr => io::stderr().write_all(bytes),
            };
            if let Err(error) = result {
                replay_error = Some(format!(
                    "failed to replay captured execution stream: {error}"
                ));
            }
        }
    }
    Ok((observation, replay_error))
}

#[derive(Debug)]
struct ExecutionReport {
    disposition: RunDispositionV1,
    exit_code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    decoded_value: Option<serde_json::Value>,
    route_results: Vec<RecordedRouteResultV1>,
    validated_selection_receipt: Option<ValidatedSelectionReceiptV1>,
    result_references: Vec<RunResultReferenceV1>,
    trace: Option<RunTraceAttachmentV1>,
    trace_unavailable_reason: String,
    failure: Option<RunFailureV1>,
}

impl ExecutionReport {
    fn infrastructure_failure(stage: &str, message: String) -> Self {
        Self {
            disposition: RunDispositionV1::InfrastructureFailed,
            exit_code: 1,
            stdout: Vec::new(),
            stderr: format!("error: {message}\n").into_bytes(),
            decoded_value: None,
            route_results: Vec::new(),
            validated_selection_receipt: None,
            result_references: Vec::new(),
            trace: None,
            trace_unavailable_reason:
                "execution did not begin because front-door observation setup failed".to_string(),
            failure: Some(RunFailureV1 {
                stage: stage.to_string(),
                message,
            }),
        }
    }

    fn post_execution_infrastructure_failure(stage: &str, message: String) -> Self {
        Self {
            disposition: RunDispositionV1::InfrastructureFailed,
            exit_code: 1,
            stdout: Vec::new(),
            stderr: format!("error: {message}\n").into_bytes(),
            decoded_value: None,
            route_results: Vec::new(),
            validated_selection_receipt: None,
            result_references: Vec::new(),
            trace: None,
            trace_unavailable_reason:
                "execution completed, but its validated-selection evidence could not be bound"
                    .to_string(),
            failure: Some(RunFailureV1 {
                stage: stage.to_string(),
                message,
            }),
        }
    }

    fn add_post_execution_failure(&mut self, stage: &str, error: &anyhow::Error) {
        let message = format!("{error:#}");
        if self.disposition == RunDispositionV1::Succeeded {
            self.disposition = RunDispositionV1::InfrastructureFailed;
            self.failure = Some(RunFailureV1 {
                stage: stage.to_string(),
                message: message.clone(),
            });
        } else if let Some(failure) = &mut self.failure {
            failure.message.push_str("; additionally: ");
            failure.message.push_str(&message);
        }
        self.exit_code = 1;
        self.stderr
            .extend_from_slice(format!("error: {message}\n").as_bytes());
    }
}

fn required_recording_begin_failure(
    args: &RunArgs,
    prepared: &PreparedExecutionIntentV1,
    error: &str,
) -> Result<i32> {
    let detail =
        format!("required run recording could not begin; no computation was executed: {error}");
    if !args.json {
        bail!("{detail}");
    }
    let summary = RunSummaryV1 {
        schema: RUN_SUMMARY_SCHEMA_V1.to_string(),
        run_id: None,
        input: Some(prepared.run_input_identity()),
        plan: Some(prepared.run_plan_identities()),
        disposition: RunDispositionV1::InfrastructureFailed,
        result_references: Vec::new(),
        recording: RunRecordingStatusV1::Incomplete {
            detail: detail.clone(),
        },
        failure: Some(RunFailureV1 {
            stage: "recording".to_string(),
            message: detail.clone(),
        }),
    };
    summary
        .validate()
        .map_err(anyhow::Error::msg)
        .context("front door produced an invalid pre-execution run summary")?;
    println!("{}", serde_json::to_string(&summary)?);
    eprintln!("error: {detail}");
    Ok(1)
}

fn stream_observation_begin_failure(
    args: &RunArgs,
    prepared: &PreparedExecutionIntentV1,
    error: &str,
) -> Result<i32> {
    let detail = format!(
        "execution stream observation could not begin; no computation was executed: {error}"
    );
    let summary = RunSummaryV1 {
        schema: RUN_SUMMARY_SCHEMA_V1.to_string(),
        run_id: None,
        input: Some(prepared.run_input_identity()),
        plan: Some(prepared.run_plan_identities()),
        disposition: RunDispositionV1::InfrastructureFailed,
        result_references: Vec::new(),
        recording: if args.no_record {
            RunRecordingStatusV1::Disabled
        } else {
            RunRecordingStatusV1::Incomplete {
                detail: detail.clone(),
            }
        },
        failure: Some(RunFailureV1 {
            stage: "stream_observation_setup".to_string(),
            message: detail.clone(),
        }),
    };
    summary
        .validate()
        .map_err(anyhow::Error::msg)
        .context("front door produced an invalid stream-observation summary")?;
    println!("{}", serde_json::to_string(&summary)?);
    eprintln!("error: {detail}");
    Ok(1)
}

fn run_intent(args: &RunArgs) -> Result<i32> {
    let resolved_args = match resolve_run_output_paths(args) {
        Ok(args) => args,
        Err(error) if args.json => {
            let detail = format!("{error:#}");
            emit_preflight_failure_summary(&detail)?;
            eprintln!("error: {detail}");
            return Ok(1);
        }
        Err(error) => return Err(error),
    };
    let args = &resolved_args;
    let prepared = match prepare_run(args) {
        Ok(prepared) => prepared,
        Err(error) if args.json => {
            let detail = format!("{error:#}");
            emit_preflight_failure_summary(&detail)?;
            eprintln!("error: {detail}");
            return Ok(1);
        }
        Err(error) => return Err(error),
    };
    // This observation is intentionally taken only after exact preflight.
    let started_unix_nanos = unix_nanos_now()?;
    let seed = prepared.run_attempt_seed(started_unix_nanos)?;
    let started = Instant::now();
    // Descriptor capture redirects stdout/stderr to private regular files.
    // Snapshot the actual invocation terminal capabilities before that
    // happens so recording cannot change ordinary evaluator presentation.
    let stdout_is_terminal = io::stdout().is_terminal();
    let stderr_is_terminal = io::stderr().is_terminal();

    let mut begin_failure = None;
    let mut lease = None;
    let capture_needed = !args.no_record || args.json;
    let mut process_capture = if capture_needed {
        match PreparedProcessCapture::prepare() {
            Ok(capture) => Some(capture),
            Err(error) if args.require_record => {
                return required_recording_begin_failure(args, &prepared, &error.to_string());
            }
            Err(error) if args.json => {
                return stream_observation_begin_failure(args, &prepared, &error.to_string());
            }
            Err(error) => {
                let detail = error.to_string();
                eprintln!(
                    "warning: run recording could not prepare process-stream observation; computation will continue unrecorded: {detail}"
                );
                begin_failure = Some(detail);
                None
            }
        }
    } else {
        None
    };
    if !args.no_record && process_capture.is_some() {
        match RunStoreV1::open_default().and_then(|store| store.begin(seed.clone())) {
            Ok(run_lease) => lease = Some(run_lease),
            Err(error) if args.require_record => {
                return required_recording_begin_failure(args, &prepared, &error.to_string());
            }
            Err(error) => {
                let detail = error.to_string();
                eprintln!(
                    "warning: run recording could not begin; computation will continue unrecorded: {detail}"
                );
                begin_failure = Some(detail);
            }
        }
    }

    let (report, recorded_stdout, recorded_stderr) = if let Some(capture) = process_capture.take() {
        match capture.execute(!args.json, || {
            execute_for_report(args, &prepared, stdout_is_terminal, stderr_is_terminal)
        }) {
            Ok((mut report, mut stdout, mut stderr, capture_error)) => {
                if let Some(error) = capture_error {
                    report.add_post_execution_failure("stream_observation", &error);
                }
                stdout.observe(&report.stdout);
                stderr.observe(&report.stderr);
                (report, stdout.finish(), stderr.finish())
            }
            Err(error) => {
                let report = ExecutionReport::infrastructure_failure(
                    "stream_observation_setup",
                    format!(
                        "execution was not started because process-stream observation failed: {error:#}"
                    ),
                );
                let recorded_stdout = CapturedStreamV1::complete(report.stdout.clone());
                let recorded_stderr = CapturedStreamV1::complete(report.stderr.clone());
                (report, recorded_stdout, recorded_stderr)
            }
        }
    } else {
        let report = execute_for_report(args, &prepared, stdout_is_terminal, stderr_is_terminal);
        let recorded_stdout = CapturedStreamV1::complete(report.stdout.clone());
        let recorded_stderr = CapturedStreamV1::complete(report.stderr.clone());
        (report, recorded_stdout, recorded_stderr)
    };
    let elapsed_nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let finished_unix_nanos = unix_nanos_now()?;

    let mut recording_diagnostic = None;
    let mut command_exit = report.exit_code;
    let summary = if let Some(lease) = lease {
        let attempt = lease.attempt().clone();
        let mut record = RunRecordV1::terminal(
            attempt.run_id.clone(),
            attempt.sequence,
            &attempt.seed,
            finished_unix_nanos,
            elapsed_nanos,
            report.disposition,
            recorded_stdout,
            recorded_stderr,
            report.decoded_value.clone(),
            report.route_results.clone(),
            report.result_references.clone(),
            RunTraceBindingV1::unavailable(report.trace_unavailable_reason.clone()),
            report.failure.clone(),
        );
        record.validated_selection_receipt = report.validated_selection_receipt.clone();
        match lease.finalize(record.clone(), report.trace.clone()) {
            Ok(finalized) => RunSummaryV1::from_record(
                &record,
                RunRecordingStatusV1::Recorded {
                    sequence: finalized.sequence,
                    record_sha256: finalized.record.sha256,
                },
            ),
            Err(error) => {
                let detail = error.to_string();
                if args.require_record {
                    command_exit = 1;
                    recording_diagnostic = Some(format!(
                        "error: required run recording failed during finalization; computation may already have occurred: {detail}\n"
                    ));
                } else {
                    recording_diagnostic = Some(format!(
                        "warning: run recording failed during finalization; preserving computation exit status: {detail}\n"
                    ));
                }
                RunSummaryV1::from_record(&record, RunRecordingStatusV1::Incomplete { detail })
            }
        }
    } else {
        RunSummaryV1 {
            schema: RUN_SUMMARY_SCHEMA_V1.to_string(),
            run_id: None,
            input: Some(prepared.run_input_identity()),
            plan: Some(prepared.run_plan_identities()),
            disposition: report.disposition,
            result_references: report.result_references.clone(),
            recording: if args.no_record {
                RunRecordingStatusV1::Disabled
            } else {
                RunRecordingStatusV1::Incomplete {
                    detail: begin_failure.unwrap_or_else(|| {
                        "run recording was unavailable before execution".to_string()
                    }),
                }
            },
            failure: report.failure.clone(),
        }
    };
    summary
        .validate()
        .map_err(anyhow::Error::msg)
        .context("front door produced an invalid run summary")?;

    if args.json {
        println!("{}", serde_json::to_string(&summary)?);
        io::stderr().write_all(&report.stderr)?;
        io::stderr().flush()?;
    } else {
        io::stdout().write_all(&report.stdout)?;
        io::stdout().flush()?;
        io::stderr().write_all(&report.stderr)?;
        io::stderr().flush()?;
    }
    if let Some(diagnostic) = recording_diagnostic {
        eprint!("{diagnostic}");
    }
    Ok(command_exit)
}

fn execute_for_report(
    args: &RunArgs,
    prepared: &PreparedExecutionIntentV1,
    stdout_is_terminal: bool,
    stderr_is_terminal: bool,
) -> ExecutionReport {
    let execution_started = Instant::now();
    let mut report = match execute_prepared_intent(prepared) {
        Ok(ExecutionObservationV1::OrdinaryO(outcome)) => {
            let stdout = render_ordinary_value_stdout_with_color(
                &outcome.value,
                !args.json && stdout_is_terminal,
            );
            let decoded_value = serde_json::to_value(&outcome.value).ok();
            let result_references =
                decoded_value_result_references(decoded_value.as_ref(), "ordinary_o");
            let (trace, trace_unavailable_reason) = {
                let PreparedExecutionIntentV1::OrdinaryO(ordinary) = prepared else {
                    unreachable!("ordinary outcome requires an ordinary prepared intent")
                };
                match OrdinaryExecutionTraceV1::from_intent_trace(
                    &outcome.trace,
                    &ordinary.identities,
                ) {
                    Ok(trace) => (
                        Some(RunTraceAttachmentV1::ordinary(trace)),
                        "ordinary evaluator trace is attached during finalization".to_string(),
                    ),
                    Err(error) => (
                        None,
                        format!("ordinary evaluator trace projection was unavailable: {error:#}"),
                    ),
                }
            };
            ExecutionReport {
                disposition: RunDispositionV1::Succeeded,
                exit_code: 0,
                stdout,
                stderr: Vec::new(),
                decoded_value,
                route_results: Vec::new(),
                validated_selection_receipt: None,
                result_references,
                trace,
                trace_unavailable_reason,
                failure: None,
            }
        }
        Ok(ExecutionObservationV1::Project(observation)) => {
            let report = project_report(
                &observation.results,
                observation.validated_selection_receipt.as_deref(),
                observation.validated_selection_measurements.as_deref(),
                observation.project_trace.as_ref(),
                observation.mesh_trace.as_ref(),
                observation.trace_unavailable_reason.as_deref(),
                args.explain_mesh,
            );
            match report {
                Ok(mut report) => {
                    if let Err(error) = write_observed_project_traces(args, &observation) {
                        report.add_post_execution_failure("trace_output", &error);
                    }
                    report
                }
                Err(error) => ExecutionReport::post_execution_infrastructure_failure(
                    "validated_selection_evidence",
                    format!("validated-selection evidence binding failed: {error:#}"),
                ),
            }
        }
        Err(error) => {
            let trace_write = write_error_traces(args, &error);
            let mut report = error_report(&error, args.explain_mesh, prepared);
            if let Err(trace_error) = trace_write {
                report.add_post_execution_failure("trace_output", &trace_error);
            }
            report
        }
    };
    if matches!(prepared, PreparedExecutionIntentV1::OrdinaryO(_))
        && report.disposition == RunDispositionV1::Succeeded
        && !args.json
        && stderr_is_terminal
    {
        let elapsed = execution_started.elapsed();
        if elapsed.as_millis() < 1000 {
            report.stderr.extend_from_slice(
                format!("\x1b[2m  {} ms\x1b[0m\n", elapsed.as_millis()).as_bytes(),
            );
        } else {
            report.stderr.extend_from_slice(
                format!("\x1b[2m  {:.2} s\x1b[0m\n", elapsed.as_secs_f64()).as_bytes(),
            );
        }
    }
    report
}

fn project_report(
    results: &[OExecutionResult],
    validated_selection_receipt: Option<&o_lang::project::ValidatedSelectionReceiptV1>,
    validated_selection_measurements: Option<&[o_lang::project::ValidatedSelectionMeasurement]>,
    project_trace: Option<&o_lang::project::ProjectAttemptTrace>,
    mesh_trace: Option<&o_lang::hosted_remote::project_mesh::MeshExecutionTraceV1>,
    unavailable_reason: Option<&str>,
    explain_mesh: bool,
) -> Result<ExecutionReport> {
    let succeeded = results.iter().any(OExecutionResult::succeeded);
    let mut stdout = Vec::new();
    for result in results {
        stdout.extend_from_slice(result.observation_summary().as_bytes());
    }
    if let Some(receipt) = validated_selection_receipt {
        let digest = receipt.sha256().unwrap_or_else(|_| "<invalid>".to_string());
        stdout.extend_from_slice(
            format!(
                "validated selection: reference={} selected={} candidates={} receipt-sha256={}\n",
                receipt.reference_route_id,
                receipt.selected_route_id,
                receipt.candidates.len(),
                digest,
            )
            .as_bytes(),
        );
    }
    let mut stderr = Vec::new();
    if explain_mesh {
        if let Some(trace) = mesh_trace {
            stderr.extend_from_slice(mesh_explanation(trace).as_bytes());
        } else {
            stderr.extend_from_slice(
                b"o mesh: no mesh trace was produced by the selected project engine\n",
            );
        }
    }
    if !succeeded {
        stderr.extend_from_slice(b"error: no project route succeeded\n");
    }
    let decoded_value = match validated_selection_receipt {
        Some(receipt) => results
            .iter()
            .find(|result| result.route_id == receipt.selected_route_id && result.succeeded())
            .and_then(|result| result.value.clone()),
        None => results
            .iter()
            .rev()
            .find(|result| result.succeeded() && result.value.is_some())
            .and_then(|result| result.value.clone()),
    };
    let mut route_results = results
        .iter()
        .map(RecordedRouteResultV1::from)
        .collect::<Vec<_>>();
    bind_validated_selection_measurements(
        &mut route_results,
        validated_selection_receipt,
        validated_selection_measurements,
    )?;
    let result_references = route_result_references(&route_results);
    let (trace, trace_unavailable_reason) = if let Some(trace) = mesh_trace {
        (
            Some(RunTraceAttachmentV1::project_mesh(trace.clone())),
            "mesh execution trace is attached during finalization".to_string(),
        )
    } else if let Some(trace) = project_trace {
        (
            Some(RunTraceAttachmentV1::project_hgraph(trace)),
            "Project HGraph trace is attached during finalization".to_string(),
        )
    } else {
        (
            None,
            unavailable_reason
                .unwrap_or("the selected compatibility project engine exposes no lifecycle trace")
                .to_string(),
        )
    };
    Ok(ExecutionReport {
        disposition: if succeeded {
            RunDispositionV1::Succeeded
        } else {
            RunDispositionV1::ExecutionFailed
        },
        exit_code: if succeeded { 0 } else { 1 },
        stdout,
        stderr,
        decoded_value,
        route_results,
        validated_selection_receipt: validated_selection_receipt.cloned(),
        result_references,
        trace,
        trace_unavailable_reason,
        failure: (!succeeded).then(|| RunFailureV1 {
            stage: "execution".to_string(),
            message: "no project route succeeded".to_string(),
        }),
    })
}

fn bind_validated_selection_measurements(
    route_results: &mut [RecordedRouteResultV1],
    receipt: Option<&ValidatedSelectionReceiptV1>,
    measurements: Option<&[o_lang::project::ValidatedSelectionMeasurement]>,
) -> Result<()> {
    let (Some(receipt), Some(measurements)) = (receipt, measurements) else {
        if receipt.is_some() || measurements.is_some() {
            bail!("validated-selection receipt and independent measurements must appear together");
        }
        return Ok(());
    };
    receipt.validate().map_err(anyhow::Error::msg)?;
    if receipt.candidates.len() != route_results.len() || measurements.len() != route_results.len()
    {
        bail!("validated-selection result, receipt, and measurement cardinalities differ");
    }

    for candidate in &receipt.candidates {
        let matching_results = route_results
            .iter()
            .filter(|result| result.route_id == candidate.route_id)
            .count();
        let matching_measurements = measurements
            .iter()
            .filter(|measurement| measurement.route_id == candidate.route_id)
            .count();
        if matching_results != 1 || matching_measurements != 1 {
            bail!(
                "validated-selection candidate `{}` lacks a unique result/measurement binding",
                candidate.route_id
            );
        }
        let measurement = measurements
            .iter()
            .find(|measurement| measurement.route_id == candidate.route_id)
            .expect("unique measurement count was checked");
        let result = route_results
            .iter_mut()
            .find(|result| result.route_id == candidate.route_id)
            .expect("unique result count was checked");
        if result.duration_ns != candidate.terminal_elapsed_ns
            || measurement.branch_elapsed_ns.to_string() != candidate.branch_elapsed_ns
            || measurement.result_codec != candidate.observation.result_codec
        {
            bail!(
                "validated-selection candidate `{}` receipt disagrees with independent runtime evidence",
                candidate.route_id
            );
        }
        result.result_codec = Some(measurement.result_codec);
        result.branch_elapsed_ns = Some(measurement.branch_elapsed_ns.to_string());
    }
    Ok(())
}

fn error_report(
    error: &anyhow::Error,
    explain_mesh: bool,
    prepared: &PreparedExecutionIntentV1,
) -> ExecutionReport {
    if let Some(ordinary) = error.downcast_ref::<OrdinaryOExecutionErrorV1>() {
        let message = format!("{error:#}");
        let (trace, trace_unavailable_reason) = {
            let PreparedExecutionIntentV1::OrdinaryO(prepared) = prepared else {
                unreachable!("ordinary failure requires an ordinary prepared intent")
            };
            match OrdinaryExecutionTraceV1::from_intent_trace(&ordinary.trace, &prepared.identities)
            {
                Ok(trace) => (
                    Some(RunTraceAttachmentV1::ordinary(trace)),
                    "ordinary evaluator failure trace is attached during finalization".to_string(),
                ),
                Err(trace_error) => (
                    None,
                    format!("ordinary failure trace projection was unavailable: {trace_error:#}"),
                ),
            }
        };
        return ExecutionReport {
            disposition: RunDispositionV1::ExecutionFailed,
            exit_code: 1,
            stdout: Vec::new(),
            stderr: format!("error: {message}\n").into_bytes(),
            decoded_value: None,
            route_results: Vec::new(),
            validated_selection_receipt: None,
            result_references: Vec::new(),
            trace,
            trace_unavailable_reason,
            failure: Some(RunFailureV1 {
                stage: "execution".to_string(),
                message,
            }),
        };
    }

    let mesh = error.downcast_ref::<MeshExecutionError>();
    let project = project_error(error);
    let message = if let Some(project) = project {
        project.public_message().to_string()
    } else if let Some(mesh) = mesh {
        mesh.public_message().to_string()
    } else {
        public_route_execution_diagnostic(error)
    };
    let results = project
        .map(|failure| {
            failure
                .settled_results()
                .map(|(_, result)| result.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut report = project_report(
        &results,
        None,
        None,
        project.map(|failure| &failure.trace),
        mesh.map(|failure| &failure.trace),
        Some("execution failed before a validated engine trace was available"),
        explain_mesh,
    )
    .expect("a project error report carries no validated-selection evidence to bind");
    let semantic = project
        .is_some_and(|failure| failure.class() == ProjectExecutionFailureClass::Semantic)
        || mesh.is_some_and(|failure| failure.class() == MeshExecutionFailureClass::Semantic);
    report.disposition = if semantic {
        RunDispositionV1::ExecutionFailed
    } else {
        RunDispositionV1::InfrastructureFailed
    };
    report.exit_code = 1;
    report
        .stderr
        .extend_from_slice(format!("error: {message}\n").as_bytes());
    report.failure = Some(RunFailureV1 {
        stage: if semantic {
            "execution"
        } else {
            "infrastructure"
        }
        .to_string(),
        message,
    });
    report
}

fn project_error(error: &anyhow::Error) -> Option<&ProjectExecutionError> {
    error.downcast_ref::<ProjectExecutionError>().or_else(|| {
        error
            .downcast_ref::<MeshExecutionError>()
            .and_then(|mesh| mesh.source_error().downcast_ref::<ProjectExecutionError>())
    })
}

fn mesh_explanation(trace: &o_lang::hosted_remote::project_mesh::MeshExecutionTraceV1) -> String {
    let mut output = format!(
        "o mesh: observed {} authenticated candidate(s) for target {} under {} policy\n",
        trace
            .candidates
            .iter()
            .filter(|candidate| candidate.eligible)
            .count(),
        trace.target,
        trace.policy
    );
    for candidate in &trace.candidates {
        output.push_str(&format!(
            "o mesh: candidate {} eligible={} slots={} latency={}us detail={}\n",
            candidate.node_id,
            candidate.eligible,
            candidate.available_slots,
            candidate.observed_latency_micros,
            candidate.detail
        ));
    }
    for event in &trace.events {
        match event {
            MeshTraceEventV1::Dispatched {
                route_id,
                actor_id,
                generation,
                node_id,
            } => output.push_str(&format!(
                "o mesh: dispatched route {route_id} actor={actor_id} generation={generation} node={node_id}\n"
            )),
            MeshTraceEventV1::Settled {
                route_id,
                actor_id,
                generation,
                node_id,
                succeeded,
            } => output.push_str(&format!(
                "o mesh: settled route {route_id} actor={actor_id} generation={generation} node={node_id} succeeded={succeeded}\n"
            )),
            MeshTraceEventV1::AttemptFailed {
                route_id,
                actor_id,
                generation,
                node_id,
                submitted,
                delivery,
                replay_contract,
                reason,
            } => output.push_str(&format!(
                "o mesh: attempt failed route {route_id} actor={actor_id} generation={generation} node={node_id} submitted={submitted} delivery={delivery} replay={replay_contract}: {reason}\n"
            )),
            MeshTraceEventV1::Migrated {
                route_id,
                actor_id,
                from_generation,
                to_generation,
                from_node_id,
                to_node_id,
                replay_contract,
            } => output.push_str(&format!(
                "o mesh: migrated route {route_id} actor={actor_id} generation={from_generation}->{to_generation} node={from_node_id}->{to_node_id} replay={replay_contract}\n"
            )),
            MeshTraceEventV1::RetryDenied {
                route_id,
                actor_id,
                generation,
                reason,
            } => output.push_str(&format!(
                "o mesh: retry denied route {route_id} actor={actor_id} after generation={generation}: {reason}\n"
            )),
            MeshTraceEventV1::LocalFallback {
                route_id,
                actor_id,
                after_generation,
                replay_contract,
                reason,
            } => output.push_str(&format!(
                "o mesh: local fallback route {route_id} actor={actor_id} after generation={after_generation} replay={replay_contract}: {reason}\n"
            )),
        }
    }
    output
}

fn unix_nanos_now() -> Result<u64> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_nanos();
    u64::try_from(nanos).context("Unix timestamp does not fit the run-record v1 field")
}

fn target_is_project(input: &Path) -> bool {
    input.is_dir()
        || fs::read_to_string(input)
            .ok()
            .is_some_and(|source| o_lang::project::lower::has_embedded_bundle(&source))
}

fn plan_has_mesh_tuning(args: &PlanArgs) -> bool {
    args.mesh_discovery_timeout_ms.is_some()
        || args.mesh_no_lan_discovery
        || args.mesh_peer_root.is_some()
}

fn plan_prepare_options(args: &PlanArgs) -> Result<PrepareExecutionOptionsV1> {
    let mesh = if plan_has_mesh_tuning(args) {
        if !target_is_project(&args.target) {
            bail!("live mesh discovery controls require a project directory or lifted project");
        }
        let mut config = MeshExecutionConfig::default();
        if let Some(timeout) = args.mesh_discovery_timeout_ms {
            config.discovery_timeout = Duration::from_millis(timeout);
        }
        config.discover_lan = !args.mesh_no_lan_discovery;
        config.peer_root = args.mesh_peer_root.clone();
        Some(config)
    } else {
        None
    };
    Ok(PrepareExecutionOptionsV1 {
        route: args.route.clone(),
        route_policy: checked_route_policy(args.routes_policy.as_deref())?,
        route_declarations: args.route_decls.clone(),
        parallel_auto: args.parallel == Some(ParallelMode::Auto),
        explicit_mesh: false,
        mesh,
        ordinary_executor: None,
        local_workers: args.workers,
        backend_grants: args.backend_grants.clone(),
        shim_dir: resolve_shim_dir(args.shim_dir.as_deref(), None)?,
        ..PrepareExecutionOptionsV1::default()
    })
}

fn plan_intent(args: &PlanArgs) -> Result<i32> {
    if args.explain_schedule {
        bail!(
            "the root static plan already includes the OIR/HGraph execution view; use direct `olangc INPUT --target ir --explain-schedule` for admission-detail formatting"
        );
    }
    if args.grounding || args.world_id.is_some() || args.world_epoch.is_some() {
        bail!("grounding/world reporting is outside the authority-free intent-plan seam");
    }
    let prepared = prepare_execution_intent(&args.target, plan_prepare_options(args)?)?;
    if args.shim_dir.is_some() && matches!(prepared, PreparedExecutionIntentV1::Project(_)) {
        bail!("--shim-dir is available only for ordinary .O planning; project routes carry their own runtime declarations");
    }
    let execution_intent = if args.execution_intent_json {
        match &prepared {
            PreparedExecutionIntentV1::OrdinaryO(ordinary) => {
                Some(serde_json::to_value(&ordinary.execution_intent)?)
            }
            PreparedExecutionIntentV1::Project(_) => {
                bail!("--execution-intent-json is available only for ordinary .O input")
            }
        }
    } else {
        None
    };
    let preview = args
        .live
        .then(|| live_placement_preview(&prepared))
        .transpose()?;
    if args.json || args.format == Some(PlanFormat::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": "ostadix.intent-plan-summary/v1",
                "input": prepared.run_input_identity(),
                "engine": prepared.engine_token(),
                "static_plan": prepared.static_plan(),
                "execution_intent": execution_intent,
                "placement_preview": preview,
            }))?
        );
        return Ok(0);
    }
    write_text_block(io::stdout(), prepared.static_plan().as_bytes())?;
    if let Some(execution_intent) = execution_intent {
        println!("; ExecutionIntentV1");
        println!("{}", serde_json::to_string_pretty(&execution_intent)?);
    }
    if let Some(preview) = preview {
        println!("; PlacementPreviewV1 (read-only; not admission)");
        println!("{}", serde_json::to_string_pretty(&preview)?);
    }
    Ok(0)
}

fn write_observed_project_traces(
    args: &RunArgs,
    observation: &o_lang::intent::ProjectExecutionObservationV1,
) -> Result<()> {
    if let Some(path) = &args.project_trace_out {
        if let Some(trace) = &observation.project_trace {
            write_json_file(path, trace)?;
        } else {
            write_trace_unavailable(
                path,
                "project_hgraph",
                observation.trace_unavailable_reason.as_deref().unwrap_or(
                    "the selected project engine did not produce a Project HGraph trace",
                ),
            )?;
        }
    }
    if let Some(path) = &args.mesh_trace_out {
        if let Some(trace) = &observation.mesh_trace {
            write_json_file(path, trace)?;
        } else {
            write_trace_unavailable(
                path,
                "project_mesh",
                "the selected project engine did not execute through the mesh",
            )?;
        }
    }
    if let Some(path) = &args.selection_receipt_out {
        let receipt = observation.validated_selection_receipt.as_deref().context(
            "benchmark_validate_and_select execution returned no validated-selection receipt",
        )?;
        receipt
            .validate()
            .map_err(anyhow::Error::msg)
            .context("refusing to write an invalid validated-selection receipt")?;
        let bytes = receipt
            .canonical_bytes()
            .map_err(anyhow::Error::msg)
            .context("failed to encode validated-selection receipt")?;
        write_file_atomically(path, &bytes)?;
    }
    Ok(())
}

fn write_error_traces(args: &RunArgs, error: &anyhow::Error) -> Result<()> {
    if let Some(path) = &args.project_trace_out {
        if let Some(project) = error.downcast_ref::<ProjectExecutionError>() {
            write_json_file(path, &project.trace)?;
        } else {
            write_trace_unavailable(
                path,
                "project_hgraph",
                &format!("execution failed before a Project HGraph trace was observed: {error:#}"),
            )?;
        }
    }
    if let Some(path) = &args.mesh_trace_out {
        if let Some(mesh) = error.downcast_ref::<MeshExecutionError>() {
            mesh.trace
                .validate()
                .context("refusing to write an invalid mesh execution trace")?;
            write_json_file(path, &mesh.trace)?;
        } else {
            write_trace_unavailable(
                path,
                "project_mesh",
                &format!("execution failed before a mesh trace was observed: {error:#}"),
            )?;
        }
    }
    Ok(())
}

fn write_trace_unavailable(path: &Path, kind: &str, reason: &str) -> Result<()> {
    write_json_file(
        path,
        &serde_json::json!({
            "schema": "ostadix.trace-unavailable/v1",
            "kind": kind,
            "reason": reason,
        }),
    )
}

fn write_json_file(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_file_atomically(path, &bytes)
}

fn write_file_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("output path must end in a UTF-8 file name")?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(
        ".{file_name}.o-write-{}-{nonce}.tmp",
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let write_result = (|| -> Result<()> {
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temporary.display()))?;
        drop(file);
        fs::rename(&temporary, path).with_context(|| {
            format!(
                "failed to atomically publish {} as {}",
                temporary.display(),
                path.display()
            )
        })?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("failed to sync output directory {}", parent.display()))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn write_text_block(mut output: impl Write, bytes: &[u8]) -> Result<()> {
    output.write_all(bytes)?;
    if !bytes.ends_with(b"\n") {
        output.write_all(b"\n")?;
    }
    output.flush()?;
    Ok(())
}

fn explain_pending(args: &ExplainArgs) -> Result<i32> {
    let selector = parse_run_selector(&args.run)?;
    let reader = RunStoreReaderV1::open_default_existing()
        .context("failed to open the private run-record store for read-only explanation")?;
    let (record, trace) = reader
        .read_terminal(selector, true)
        .context("failed to resolve and verify retained run evidence")?;
    let narrative = explain_verified_run(&record, trace.as_ref())?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": "ostadix.run-explanation/v1",
                "narrative": narrative,
                "record": record,
                "trace": trace,
            }))?
        );
    } else {
        print!("{narrative}");
    }
    Ok(0)
}

fn inspect_pending(args: &InspectArgs) -> Result<i32> {
    let selector = parse_run_selector(&args.run)?;
    let reader = RunStoreReaderV1::open_default_existing()
        .context("failed to open the private run-record store for read-only inspection")?;
    let inspection = reader
        .inspect(selector, args.trace)
        .context("failed to resolve and verify retained run evidence")?;
    // `inspect` is a JSON evidence command by contract. `--json` remains an
    // accepted compatibility spelling and intentionally changes no semantics.
    let _json_requested = args.json;
    println!("{}", serde_json::to_string_pretty(&inspection)?);
    Ok(0)
}

fn parse_positive_usize(value: &str) -> std::result::Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("expected a positive integer, got `{value}`"))?;
    if parsed == 0 {
        return Err("value must be at least 1".to_string());
    }
    Ok(parsed)
}

fn parse_mesh_retries(value: &str) -> std::result::Result<u32, String> {
    let parsed = value
        .parse::<u32>()
        .map_err(|_| format!("expected an integer from 0 through 64, got `{value}`"))?;
    if parsed > 64 {
        return Err("mesh retries may not exceed 64".to_string());
    }
    Ok(parsed)
}

fn parse_discovery_timeout_ms(value: &str) -> std::result::Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("expected an integer from 1 through 60000, got `{value}`"))?;
    if !(1..=60_000).contains(&parsed) {
        return Err("mesh discovery timeout must be from 1 through 60000 ms".to_string());
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use tempfile::tempdir;

    fn parse_run(arguments: &[&str]) -> RunArgs {
        let cli = Cli::try_parse_from(arguments).unwrap();
        let IntentCommand::Run(run) = cli.command else {
            panic!("expected run command")
        };
        run
    }

    fn parse_plan(arguments: &[&str]) -> PlanArgs {
        let cli = Cli::try_parse_from(arguments).unwrap();
        let IntentCommand::Plan(plan) = cli.command else {
            panic!("expected plan command")
        };
        plan
    }

    fn parse_object(arguments: &[&str]) -> ObjectArgs {
        let cli = Cli::try_parse_from(arguments).unwrap();
        let IntentCommand::Object(object) = cli.command else {
            panic!("expected object command")
        };
        object
    }

    fn parse_computation(arguments: &[&str]) -> ComputationArgs {
        let cli = Cli::try_parse_from(arguments).unwrap();
        let IntentCommand::Computation(computation) = cli.command else {
            panic!("expected computation command")
        };
        computation
    }

    #[test]
    fn computation_command_requires_explicit_artifact_and_binary_paths() {
        let computation = parse_computation(&[
            "o",
            "computation",
            "--source",
            "program.O",
            "--execution-intent",
            "execution-intent.json",
            "--schedule",
            "schedule.txt",
            "--hgraph-dot",
            "hgraph.dot",
            "--result",
            "result.json",
            "--o-bin",
            "O",
            "--olangc-bin",
            "olangc",
            "--cbor-out",
            "computation.cbor",
            "--json-out",
            "computation.json",
        ]);
        assert_eq!(computation.source, Path::new("program.O"));
        assert_eq!(
            computation.execution_intent,
            Path::new("execution-intent.json")
        );
        assert_eq!(computation.lineage, "examples/semantic-custody");
    }

    #[test]
    fn boot_object_commands_are_read_only_and_accept_a_global_store() {
        let object = parse_object(&[
            "o",
            "object",
            "verify",
            "--store",
            "/usr/share/ostadix/boot-objects/v1",
        ]);
        assert_eq!(
            object.store.as_deref(),
            Some(Path::new("/usr/share/ostadix/boot-objects/v1"))
        );
        assert!(matches!(object.command, ObjectCommand::Verify(_)));

        for command in ["root", "list", "stat", "get", "verify"] {
            let help = Cli::try_parse_from(["o", "object", command, "--help"]).unwrap_err();
            assert_eq!(help.kind(), ErrorKind::DisplayHelp);
        }
    }

    #[test]
    fn boot_object_list_prefix_respects_path_component_boundaries() {
        assert!(path_matches_prefix("src", "src"));
        assert!(path_matches_prefix("src/bin/o-cli.rs", "src"));
        assert!(path_matches_prefix("src/bin/o-cli.rs", "src/"));
        assert!(!path_matches_prefix("src2/main.rs", "src"));
        assert!(!path_matches_prefix("source/main.rs", "src"));
        assert!(path_matches_prefix("anything", ""));
    }

    #[test]
    fn run_options_are_accepted_before_and_after_the_target() {
        let before = parse_run(&[
            "o",
            "run",
            "--parallel",
            "auto",
            "--route",
            "build",
            "project",
        ]);
        let after = parse_run(&[
            "o",
            "run",
            "project",
            "--route",
            "build",
            "--parallel",
            "auto",
        ]);
        assert_eq!(before.target, after.target);
        assert_eq!(before.parallel, after.parallel);
        assert_eq!(before.route, after.route);
    }

    #[test]
    fn ordinary_auto_run_selects_the_local_graph_without_mesh() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("program.O");
        let backends = temp.path().join("backends");
        fs::write(&source, b"text^(ok)_text\n").unwrap();
        fs::create_dir(&backends).unwrap();
        let run = parse_run(&[
            "o",
            "run",
            source.to_str().unwrap(),
            backends.to_str().unwrap(),
            "--parallel",
            "auto",
            "--workers",
            "3",
            "--no-record",
        ]);
        let prepared = prepare_run(&run).unwrap();
        let PreparedExecutionIntentV1::OrdinaryO(ordinary) = prepared else {
            panic!("ordinary source was classified as a project")
        };
        assert_eq!(ordinary.executor, LocalOExecutorV1::ForcedGraph);
        assert_eq!(ordinary.local_workers, Some(3));
        assert_eq!(ordinary.shim_dir, backends);
        assert!(ordinary.parallel_auto);
    }

    #[test]
    fn project_auto_and_explicit_mesh_are_rejected_before_filesystem_access() {
        let run = parse_run(&[
            "o",
            "run",
            "project",
            "--project",
            "--parallel",
            "auto",
            "--route",
            "build",
            "--routes-policy",
            "all",
            "--mesh=required",
            "--mesh-retries=4",
            "--mesh-local-fallback=never",
            "--mesh-discovery-timeout-ms=900",
            "--mesh-no-lan-discovery",
            "--mesh-peer-root=peers",
            "--mesh-trace-out=trace.json",
            "--explain-mesh",
            "--no-record",
        ]);
        let options = run_prepare_options(&run).unwrap();
        assert!(options.parallel_auto);
        assert!(options.explicit_mesh);
        let mesh = options.mesh.as_ref().unwrap();
        assert_eq!(mesh.requirement, MeshRequirement::Required);
        assert_eq!(mesh.max_retries, 4);
        assert_eq!(mesh.local_fallback, MeshLocalFallback::Never);
        assert_eq!(mesh.discovery_timeout, Duration::from_millis(900));
        assert!(!mesh.discover_lan);
        assert_eq!(mesh.peer_root.as_deref(), Some(Path::new("peers")));
        assert!(!mesh.explain);
        assert!(run.explain_mesh);
        let error = prepare_execution_intent(Path::new("project"), options)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--parallel auto conflicts with explicit --mesh"));
    }

    #[test]
    fn static_and_live_ordinary_plans_use_direct_intent_views() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("program.O");
        fs::write(&source, b"text^(ok)_text\n").unwrap();
        let plan = parse_plan(&[
            "o",
            "plan",
            source.to_str().unwrap(),
            "--parallel",
            "auto",
            "--live",
            "--workers",
            "2",
        ]);
        let prepared =
            prepare_execution_intent(&plan.target, plan_prepare_options(&plan).unwrap()).unwrap();
        assert!(prepared.static_plan().contains("HGraph"));
        let preview = live_placement_preview(&prepared).unwrap();
        assert!(preview.candidates.is_empty());
        assert!(preview.selected_node_id.is_none());
    }

    #[test]
    fn mesh_tuning_requires_mesh_or_parallel_auto() {
        let run = parse_run(&["o", "run", "project", "--mesh-retries=2"]);
        assert!(run_prepare_options(&run)
            .unwrap_err()
            .to_string()
            .contains("require --parallel auto or --mesh"));
    }

    #[test]
    fn route_policies_are_checked_and_closed_registry_alias_is_public() {
        let run = parse_run(&[
            "o",
            "run",
            "project",
            "--mesh",
            "--closed-registry",
            "--route",
            "build",
            "--routes-policy",
            "defualt",
        ]);
        assert!(run.mesh_no_lan_discovery);
        assert!(run_prepare_options(&run)
            .unwrap_err()
            .to_string()
            .contains("unknown route policy"));
    }

    #[test]
    fn help_is_unified_and_missing_run_target_is_a_usage_error() {
        let help = Cli::command().render_long_help().to_string();
        for text in [
            "run",
            "plan",
            "explain",
            "inspect",
            "object",
            "root|list|stat|get|verify",
            "closed-registry",
            "node start|stop|status|restart",
            "Unknown command forms retain historical evaluator behavior",
        ] {
            assert!(help.contains(text), "help omitted {text:?}:\n{help}");
        }

        let error = Cli::try_parse_from(["o", "run"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }
}
