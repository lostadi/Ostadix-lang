// ─────────────────────────────────────────────────────────────────────────────
// o-link: the Ostadix-lang linker / combiner compiler
//
// Accepts explicit scripts/source files or whole codebases and links them into
// a single .O file. A bare single-directory invocation uses the full literal
// function by default: it links every selected UTF-8 file and immediately runs
// the resulting program. Safe lossless project lifting remains available with
// the explicit --project flag.
//
// In literal mode, each input file is wrapped in the typed-expression block of
// the backend that matches its extension:
//
//   hello.py    →  python[*]^( ...file contents... )_python[*]
//   build.sh    →  bash[*]^( ...file contents... )_bash[*]
//   index.html  →  html[*]^( ...file contents... )_html[*]
//   notes.md    →  markdown[*]^( ... )_markdown[*]
//   prog.O      →  inlined verbatim (it is already Ostadix-lang source)
//
// Every wrapped file receives the explicit `[*]` fresh-environment marker.
// Unlike an authored numeric `[N]`, this is placement-eligible and cannot
// collide across aliases or with an environment already present in an inlined
// `.O` source.
//
// Files of the same language are ordered by their import-dependency graph
// before wrapping: if `b.py` imports from `a.py`, `a.py` will appear first,
// regardless of alphabetical order. For languages without import scanning support,
// files keep the sorted order from the directory walk.
//
// Literal directories are walked recursively; every UTF-8 text file is
// included in sorted order so the output is deterministic. Unknown and
// extensionless files use the inert text backend unless --lang selects another
// backend. Explicit project mode instead captures the whole tree losslessly
// and executes only an explicitly selected discovered/manifest route.
//
// Any text inside a wrapped file that would collide with Ostadix-lang syntax:
// a registered opener like `python^(`, the wrapping block's own closer
// like `)_python`, or a splice like `$HOME`, is backslash-escaped
// (`\python^(`, `\)_python`, `\$HOME`), which the Ostadix-lang parser turns
// back into the literal text at evaluation time, so file contents survive
// the round trip byte-for-byte.
//
// Usage:
//   o-link a.py b.sh c.html -o program.O      # link three scripts
//   o-link src/                                # link + run every selected source
//   o-link src/ --literal -o sequential.O     # literal link only (do not run)
//   o-link src/ --project -o project.O        # safely lift a whole codebase
//   o-link src/ --project --run                # run its project default route
//   o-link src/ --run --mesh=prefer            # place its default route on the peer mesh
//   o-link a.py --lang txt=markdown -o out.O  # extra extension mapping
//   o-link a.py --stdout                      # write to stdout instead
//   o-link a.py b.sh --run                    # link, then execute in-process
//   o-link src/ --project -o app.O --shebang  # safe lifted project + shebang
//   o-link src/ --literal --verbose-skips      # report literal-mode exclusions
//
// Robustness guarantees:
//   * The combined output is re-parsed with the Ostadix-lang parser before it is
//     written, so o-link never emits a .O file that the runtime cannot read.
//   * Directory walks skip binary / non-UTF-8 files, group warnings by reason,
//     do not descend into excluded subtrees unless --verbose-skips is set,
//     follow symlinked directories at most once (no infinite loops), and never
//     pick up the output file itself.
//   * The same file given twice (directly or via overlapping directories) is
//     linked only once.
// ─────────────────────────────────────────────────────────────────────────────

use anyhow::{bail, Context, Result};
use clap::{Parser as ClapParser, ValueEnum};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

use o_lang::eval::Evaluator;
use o_lang::parser::Parser;
use o_lang::value::OValue;

const SECTION_LENGTH_PREFIX: &str = "# o-link-section-bytes: ";
const O_LINK_GENERATED_HEADER: &str = "# Linked by o-link";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ParallelLinkMode {
    /// Explicitly authorize fresh hosted blocks to overlap despite unknown
    /// hidden host effects. Explicit O-value dependencies remain ordered.
    Autonomous,
    /// Overlap only catalog-verified pure inline renderers; hosted shims remain
    /// sequential in their original positions.
    Verified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MeshMode {
    /// Prefer an eligible peer, with policy-controlled local fallback.
    Prefer,
    /// Require mesh placement; fail when no eligible peer can execute the work.
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MeshLocalFallbackMode {
    /// Fall back locally only when actor execution is proven not to have begun.
    PreSend,
    /// Also permit local replay when the bundle declares the route idempotent.
    Idempotent,
    /// Never fall back to local execution after mesh placement is requested.
    Never,
}

impl ParallelLinkMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Autonomous => "autonomous",
            Self::Verified => "verified",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkippedPath {
    path: PathBuf,
    reason: String,
}

#[derive(Debug)]
struct CollectedFiles {
    files: Vec<PathBuf>,
    marker_root: PathBuf,
    skipped: Vec<SkippedPath>,
}

impl CollectedFiles {
    fn report_lines(&self, verbose_skips: bool) -> Vec<String> {
        let mut lines = Vec::new();
        if verbose_skips {
            lines.extend(self.skipped.iter().map(|skipped| {
                format!(
                    "warning: skipped {} ({})",
                    skipped.path.display(),
                    skipped.reason
                )
            }));
        } else {
            let mut counts = BTreeMap::<&str, usize>::new();
            for skipped in &self.skipped {
                *counts.entry(&skipped.reason).or_default() += 1;
            }
            lines.extend(counts.into_iter().map(|(reason, count)| {
                let noun = if count == 1 { "path" } else { "paths" };
                format!("warning: skipped {count} {noun} ({reason})")
            }));
        }
        lines.push(format!(
            "o-link scan: {} selected, {} skipped",
            self.files.len(),
            self.skipped.len()
        ));
        lines
    }

    fn report(&self, verbose_skips: bool) {
        for line in self.report_lines(verbose_skips) {
            eprintln!("{line}");
        }
    }
}

#[derive(Debug)]
struct IgnoreRules {
    source: PathBuf,
    matcher: Gitignore,
}

struct WalkState<'a> {
    exclude: Option<&'a Path>,
    seen_files: &'a mut HashSet<PathBuf>,
    seen_dirs: &'a mut HashSet<PathBuf>,
    files: &'a mut Vec<PathBuf>,
    skipped: &'a mut Vec<SkippedPath>,
    ignore_rules: &'a mut Vec<IgnoreRules>,
    enumerate_excluded_trees: bool,
}

/// o-link links multiple scripts or codebases into a single .O file.
#[derive(Debug, ClapParser)]
#[command(
    name = "o-link",
    about = "Link scripts into one .O program; bare directories link and run by default"
)]
struct Cli {
    /// Input files and/or directories. A bare single directory is linked
    /// literally and run; use --literal for link-only or --project for a safe
    /// route-preserving bundle.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Output path for the combined .O file.
    #[arg(short = 'o', long = "output", default_value = "combined.O")]
    output: PathBuf,

    /// Write the combined source to stdout instead of a file.
    #[arg(long = "stdout", conflicts_with = "output")]
    to_stdout: bool,

    /// Extra extension→backend mappings, e.g. --lang txt=markdown.
    /// May be given multiple times; overrides the built-in mapping.
    #[arg(long = "lang", value_name = "EXT=BACKEND")]
    lang: Vec<String>,

    /// Print one warning for every skipped path instead of grouping by reason.
    #[arg(long)]
    verbose_skips: bool,

    /// Skip the parse-validation pass on the combined output.
    #[arg(long = "no-validate")]
    no_validate: bool,

    /// Execute the combined program in-process after linking. This is inferred
    /// for a bare single-directory invocation.
    #[arg(long = "run")]
    run: bool,

    /// Shim directory used by execution. Defaults to O_BACKENDS_DIR,
    /// BACKENDS_DIR, then ./backends.
    #[arg(long = "shim-dir")]
    shim_dir: Option<PathBuf>,

    /// Compatibility hook for --run; normal shim backends already have default
    /// host authority. Format:
    /// `NAME=LANG[:fs_read,fs_write,network,process]`.
    #[arg(long = "backend-grant")]
    backend_grants: Vec<String>,

    /// Prepend `#!/usr/bin/env o` and mark the output executable, so the
    /// combined .O file can be run directly (`./program.O`).
    #[arg(long = "shebang", conflicts_with = "to_stdout")]
    shebang: bool,

    /// Safely lift a directory as a route-preserving project bundle. With
    /// --run, execute only the selected/default project route.
    #[arg(long = "project")]
    project: bool,

    /// Wrap every selected UTF-8 file as an executable backend block without
    /// implicitly running it. Required for mixed/multiple directory inputs.
    /// Add --run to execute every wrapped source file in order.
    #[arg(long, visible_alias = "execute-all")]
    literal: bool,

    /// Print the discovered + manifest route table for the input directory or
    /// an existing lifted .O file, then exit without executing anything.
    #[arg(long = "list-routes")]
    list_routes: bool,

    /// With `--run`, execute this route (or route set, by its `provides`
    /// token) through the project runtime.
    #[arg(long = "route", value_name = "ID", requires = "run")]
    route: Option<String>,

    /// With `--run`, apply this policy to the selected route set.
    /// One of: explicit, default, fallback, any_success, race_success,
    /// race_settle, all, verify_equivalent, benchmark_and_select.
    #[arg(long = "routes-policy", value_name = "POLICY", requires = "run")]
    routes_policy: Option<String>,

    /// Write the unsigned Project HGraph attempt trace as JSON. Requires
    /// hosted project execution through --run and O_PROJECT_EXECUTOR=hgraph.
    #[arg(long = "project-trace-out", value_name = "PATH", requires = "run")]
    project_trace_out: Option<PathBuf>,

    /// Add or override a route from the command line (repeatable). Micro-syntax:
    /// `id=NAME;cmd=PROGRAM ARGS;cwd=.;provides=a,b;codec=json;depends=r1,r2`.
    #[arg(long = "route-decl", value_name = "DECL")]
    route_decls: Vec<String>,

    /// Emit placement-eligible execution groups. With no value this selects
    /// `autonomous`; use `--parallel=verified` to overlap only verified pure
    /// inline renderers.
    #[arg(
        long,
        value_enum,
        num_args = 0..=1,
        default_missing_value = "autonomous"
    )]
    parallel: Option<ParallelLinkMode>,

    /// Fail if any selected section cannot enter the requested parallel lane.
    #[arg(long = "parallel-required", requires = "parallel")]
    parallel_required: bool,

    /// Explain each section's parallel-placement decision on stderr.
    #[arg(long = "explain-parallel")]
    explain_parallel: bool,

    /// Execute a project route through the peer mesh. With no value, prefer an
    /// eligible peer; use `--mesh=required` to fail instead of accepting policy-
    /// controlled local fallback when no peer can execute the route.
    #[arg(
        long,
        value_enum,
        num_args = 0..=1,
        default_missing_value = "prefer",
        require_equals = true,
        requires = "run",
        conflicts_with_all = ["literal", "project_trace_out"]
    )]
    mesh: Option<MeshMode>,

    /// Maximum mesh retries after the first attempt.
    #[arg(
        long = "mesh-retries",
        default_value_t = 2,
        value_parser = clap::value_parser!(u32).range(0..=64),
        requires_all = ["mesh", "run"]
    )]
    mesh_retries: u32,

    /// Policy for falling back to local project execution after mesh failure.
    #[arg(
        long = "mesh-local-fallback",
        value_enum,
        default_value = "pre-send",
        requires_all = ["mesh", "run"]
    )]
    mesh_local_fallback: MeshLocalFallbackMode,

    /// Time allowed for automatic peer discovery before target selection.
    #[arg(
        long = "mesh-discovery-timeout-ms",
        default_value_t = 750,
        value_parser = clap::value_parser!(u64).range(1..=60_000),
        requires_all = ["mesh", "run"]
    )]
    mesh_discovery_timeout_ms: u64,

    /// Use only the explicit paired-peer registry; do not send or accept live
    /// UDP LAN discovery advertisements for this invocation.
    #[arg(
        long = "mesh-no-lan-discovery",
        requires_all = ["mesh", "run"]
    )]
    mesh_no_lan_discovery: bool,

    /// Root containing paired peer records and transport credentials.
    #[arg(
        long = "mesh-peer-root",
        value_name = "PATH",
        requires_all = ["mesh", "run"]
    )]
    mesh_peer_root: Option<PathBuf>,

    /// Write the mesh scheduler/placement attempt trace as JSON.
    #[arg(
        long = "mesh-trace-out",
        value_name = "PATH",
        requires_all = ["mesh", "run"]
    )]
    mesh_trace_out: Option<PathBuf>,

    /// Explain mesh discovery, eligibility, target choice, retries, and fallback
    /// decisions on stderr while executing the project route.
    #[arg(long = "explain-mesh", requires_all = ["mesh", "run"])]
    explain_mesh: bool,
}

fn has_project_intent(cli: &Cli) -> bool {
    cli.project
        || cli.list_routes
        || cli.route.is_some()
        || cli.routes_policy.is_some()
        || cli.project_trace_out.is_some()
        || !cli.route_decls.is_empty()
        || cli.mesh.is_some()
}

fn mesh_execution_config(
    cli: &Cli,
) -> Option<o_lang::hosted_remote::project_mesh::MeshExecutionConfig> {
    use o_lang::hosted_remote::project_mesh::{MeshLocalFallback, MeshRequirement};

    let requirement = match cli.mesh? {
        MeshMode::Prefer => MeshRequirement::Prefer,
        MeshMode::Required => MeshRequirement::Required,
    };
    let local_fallback = match cli.mesh_local_fallback {
        MeshLocalFallbackMode::PreSend => MeshLocalFallback::PreSend,
        MeshLocalFallbackMode::Idempotent => MeshLocalFallback::Idempotent,
        MeshLocalFallbackMode::Never => MeshLocalFallback::Never,
    };

    Some(o_lang::hosted_remote::project_mesh::MeshExecutionConfig {
        requirement,
        max_retries: cli.mesh_retries,
        local_fallback,
        discover_lan: !cli.mesh_no_lan_discovery,
        discovery_timeout: std::time::Duration::from_millis(cli.mesh_discovery_timeout_ms),
        peer_root: cli.mesh_peer_root.clone(),
        trace_out: cli.mesh_trace_out.clone(),
        explain: cli.explain_mesh,
    })
}

fn main() -> Result<()> {
    if o_lang::backend::run_backend_from_env_args()? {
        return Ok(());
    }

    let mut cli = Cli::parse();
    let backends = registered_backends();

    // ── Default literal execution vs explicit project mode ───────────────────
    // A bare single directory intentionally selects the full literal function:
    // link every selected UTF-8 file and run the combined program. `--literal`
    // keeps the same linker but explicitly suppresses the inferred run, while
    // `--project` retains the lossless, route-preserving safe path.
    if cli.literal && (cli.project || cli.list_routes) {
        bail!("--literal/--execute-all cannot be combined with --project or --list-routes");
    }
    if cli.list_routes && cli.run {
        bail!("--list-routes cannot be combined with --run");
    }
    if (cli.route.is_some() || cli.routes_policy.is_some()) && !cli.run {
        bail!("--route and --routes-policy require --run");
    }

    let project_intent = has_project_intent(&cli);
    let implicit_literal_run =
        !project_intent && !cli.literal && cli.inputs.len() == 1 && cli.inputs[0].is_dir();
    if implicit_literal_run {
        if cli.to_stdout {
            bail!(
                "bare single-directory mode runs by default and cannot be combined with --stdout; add --literal for link-only output or --project for a safe project bundle"
            );
        }
        cli.literal = true;
        cli.run = true;
        eprintln!("warning: bare single-directory mode defaults to --literal --run");
        eprintln!(
            "warning: every selected executable backend block will run; use --project for safe project lifting or --literal for literal link-only output"
        );
    }

    if !cli.backend_grants.is_empty() && !cli.run {
        bail!("--backend-grant requires --run");
    }

    if cli.list_routes {
        ensure_project_compatible_flags(&cli)?;
        return list_routes_mode(&cli);
    }

    let implicit_project = implicit_lifted_project_input(&cli)?;
    if project_intent || implicit_project {
        ensure_project_compatible_flags(&cli)?;
        return project_mode(&cli);
    }

    if !cli.literal && cli.inputs.iter().any(|input| input.is_dir()) {
        bail!(
            "multiple or mixed directory inputs require --literal; use --project with exactly one directory for safe project lifting"
        );
    }

    if !implicit_literal_run && cli.literal && cli.inputs.iter().any(|input| input.is_dir()) {
        eprintln!(
            "warning: --literal/--execute-all directory mode wraps every selected UTF-8 file as executable backend code"
        );
        if let Some(mode) = cli.parallel {
            eprintln!(
                "warning: {} parallel groups preserve result order, but admitted members have unordered hidden effects",
                mode.label()
            );
        } else {
            eprintln!(
                "warning: running the linked document executes all wrapped backend blocks in dependency order"
            );
        }
    }

    let mut ext_map = default_extension_map();
    for spec in &cli.lang {
        let (ext, backend) = spec
            .split_once('=')
            .with_context(|| format!("--lang expects EXT=BACKEND, got `{}`", spec))?;
        if !backends.contains(backend) {
            bail!("--lang {}: `{}` is not a registered backend", spec, backend);
        }
        ext_map.insert(ext.trim_start_matches('.').to_string(), backend.to_string());
    }

    // Never let the output file get linked into itself when a directory walk
    // would otherwise reach it (e.g. `o-link . -o ./combined.O` run twice).
    let exclude = (!cli.to_stdout)
        .then(|| path_identity(&cli.output))
        .transpose()?;

    let collected =
        collect_files_with_skip_mode(&cli.inputs, &ext_map, exclude.as_deref(), cli.verbose_skips)?;
    collected.report(cli.verbose_skips);
    if collected.files.is_empty() {
        bail!("no linkable files found in the given inputs");
    }

    let mut combined = link_files_with_options(
        &collected.files,
        &collected.marker_root,
        &ext_map,
        &backends,
        cli.parallel,
        cli.parallel_required,
        cli.explain_parallel,
    )?;

    if !cli.no_validate {
        let mut parser = Parser::new(&combined, &backends);
        parser
            .parse()
            .context("internal error: combined output does not parse as .O source")?;
    }

    if cli.shebang {
        combined.insert_str(0, "#!/usr/bin/env o\n");
    }

    if cli.to_stdout {
        print!("{}", combined);
    } else {
        fs::write(&cli.output, &combined)
            .with_context(|| format!("failed to write {}", cli.output.display()))?;
        if cli.shebang {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&cli.output, fs::Permissions::from_mode(0o755)).with_context(
                    || format!("failed to mark {} executable", cli.output.display()),
                )?;
            }
        }
        eprintln!(
            "linked {} file(s) into {}",
            collected.files.len(),
            cli.output.display()
        );
    }

    if cli.run {
        run_combined(
            &combined,
            resolve_shim_dir(cli.shim_dir),
            backends,
            &cli.backend_grants,
        )?;
    }

    Ok(())
}

fn resolve_shim_dir(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| {
            env::var_os("O_BACKENDS_DIR")
                .or_else(|| env::var_os("BACKENDS_DIR"))
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from("backends"))
}

/// Execute the combined program in-process, the same way the `O` interpreter
/// would: strip the shebang (if any), parse, evaluate, print the result.
fn run_combined(
    source: &str,
    shim_dir: PathBuf,
    backends: HashSet<String>,
    backend_grants: &[String],
) -> Result<()> {
    let body = if source.starts_with("#!") {
        source.find('\n').map(|nl| &source[nl + 1..]).unwrap_or("")
    } else {
        source
    };

    let mut parser = Parser::new(body, &backends);
    let nodes = parser
        .parse()
        .context("failed to parse combined .O source")?;

    let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends);
    let mut scope = HashMap::new();
    for grant in backend_grants {
        evaluator.install_backend_grant(grant, &mut scope)?;
    }
    let result = evaluator
        .eval_document_with_scope(nodes, &mut scope)
        .context("failed to evaluate combined .O program")?;

    match result {
        OValue::Html { v } => println!("{}", v),
        OValue::Text { v } => println!("{}", v.utf8),
        other => println!("{}", other),
    }
    Ok(())
}

/// True when a previously lifted project document should retain its first-class
/// project semantics without requiring the `--project` spelling again.
fn implicit_lifted_project_input(cli: &Cli) -> Result<bool> {
    if cli.literal || cli.inputs.len() != 1 {
        return Ok(false);
    }

    let input = &cli.inputs[0];
    if !input.is_file() {
        return Ok(false);
    }

    let source =
        fs::read_to_string(input).with_context(|| format!("failed to read {}", input.display()))?;
    Ok(o_lang::project::lower::has_embedded_bundle(&source))
}

/// Project mode has different semantics from literal per-file wrapping. Reject
/// options that would otherwise be silently ignored rather than surprising the
/// user or accidentally weakening explicit project semantics.
fn ensure_project_compatible_flags(cli: &Cli) -> Result<()> {
    let mut incompatible = Vec::new();
    if !cli.lang.is_empty() {
        incompatible.push("--lang");
    }
    if cli.verbose_skips {
        incompatible.push("--verbose-skips");
    }
    if cli.no_validate {
        incompatible.push("--no-validate");
    }
    if cli.shim_dir.is_some() {
        incompatible.push("--shim-dir");
    }
    if !cli.backend_grants.is_empty() {
        incompatible.push("--backend-grant");
    }
    if cli.parallel.is_some() {
        incompatible.push("--parallel");
    }
    if cli.parallel_required {
        incompatible.push("--parallel-required");
    }
    if cli.explain_parallel {
        incompatible.push("--explain-parallel");
    }

    if !incompatible.is_empty() {
        bail!(
            "{} configure literal per-file linking and cannot be used in project mode; remove --project or add --literal to select literal link-only mode",
            incompatible.join(", ")
        );
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Project mode
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve the single input for project mode, requiring exactly one path.
fn single_input(cli: &Cli) -> Result<PathBuf> {
    if cli.inputs.len() != 1 {
        bail!("project mode takes exactly one input (a directory or a lifted .O file)");
    }
    Ok(cli.inputs[0].clone())
}

/// Build a `ProjectBundle` for the single input, whether it is a directory
/// or an already-lifted `.O` file.
fn load_project_bundle(cli: &Cli) -> Result<o_lang::project::ProjectBundle> {
    let input = single_input(cli)?;
    if input.is_dir() {
        let name = o_lang::project::name_from_path(&input);
        let mut exclusions = (!cli.to_stdout && !cli.run && !cli.list_routes)
            .then(|| cli.output.clone())
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(trace_out) = &cli.project_trace_out {
            exclusions.push(trace_out.clone());
        }
        if let Some(trace_out) = &cli.mesh_trace_out {
            exclusions.push(trace_out.clone());
        }
        o_lang::project::assemble_excluding(&input, &name, &cli.route_decls, &exclusions)
    } else if input.is_file() {
        let source = fs::read_to_string(&input)
            .with_context(|| format!("failed to read {}", input.display()))?;
        if !o_lang::project::lower::has_embedded_bundle(&source) {
            bail!(
                "{}: not a project directory and not a lifted .O project file",
                input.display()
            );
        }
        let mut bundle = o_lang::project::lower::extract_bundle_from_o(&source)?;
        o_lang::project::manifest::apply_cli_overrides(&mut bundle, &cli.route_decls)?;
        o_lang::project::finalize_default(&mut bundle);
        Ok(bundle)
    } else {
        bail!("{}: no such file or directory", input.display());
    }
}

/// `--list-routes`: print the route table and exit without executing.
fn list_routes_mode(cli: &Cli) -> Result<()> {
    let bundle = load_project_bundle(cli)?;
    print!("{}", bundle.route_table());
    Ok(())
}

/// `--project`: lift into a single .O document, or (with `--run`) execute a
/// route through the project runtime.
fn project_mode(cli: &Cli) -> Result<()> {
    let bundle = load_project_bundle(cli)?;

    if cli.run {
        return run_project(cli, &bundle);
    }

    // Lift into one valid .O document.
    let mut lifted = o_lang::project::lower::lower_to_o_validated(&bundle)
        .context("failed to lift project into a .O document")?;
    if cli.shebang {
        lifted.insert_str(0, "#!/usr/bin/env o\n");
    }

    if cli.to_stdout {
        print!("{}", lifted);
    } else {
        fs::write(&cli.output, &lifted)
            .with_context(|| format!("failed to write {}", cli.output.display()))?;
        if cli.shebang {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&cli.output, fs::Permissions::from_mode(0o755))?;
            }
        }
        eprintln!(
            "lifted project '{}' ({} file(s), {} route(s)) into {}",
            bundle.name,
            bundle.files.len(),
            bundle.routes.len(),
            cli.output.display()
        );
    }
    Ok(())
}

/// Execute a route (or route set) through the project runtime.
fn run_project(cli: &Cli, bundle: &o_lang::project::ProjectBundle) -> Result<()> {
    use o_lang::hosted_remote::project_mesh::execute_mesh_selection;
    use o_lang::project::executor::{
        execute_selection_with_configured_executor, write_project_attempt_trace,
        ProjectExecutionError, PROJECT_EXECUTOR_ENV,
    };
    use o_lang::project::runtime::RunOptions;
    use o_lang::project::RoutePolicy;

    if cli.route.is_none() && cli.routes_policy.is_none() && bundle.resolved_default().is_none() {
        print!("{}", bundle.route_table());
        bail!("no unambiguous default route — select one with --route <ID>");
    }

    let policy = cli
        .routes_policy
        .as_deref()
        .map(RoutePolicy::parse_checked)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let opts = RunOptions::default();
    let mesh_config = mesh_execution_config(cli);
    if mesh_config.is_none()
        && cli.project_trace_out.is_some()
        && std::env::var_os(PROJECT_EXECUTOR_ENV).as_deref() != Some(std::ffi::OsStr::new("hgraph"))
    {
        bail!(
            "--project-trace-out requires {PROJECT_EXECUTOR_ENV}=hgraph; the legacy project runtime does not produce a Project HGraph attempt trace"
        );
    }

    let execution_result = match mesh_config.as_ref() {
        Some(config) => execute_mesh_selection(bundle, cli.route.as_deref(), policy, &opts, config),
        None => {
            execute_selection_with_configured_executor(bundle, cli.route.as_deref(), policy, &opts)
        }
    };
    let execution = match execution_result {
        Ok(execution) => execution,
        Err(error) => {
            if let (Some(path), Some(project_error)) = (
                cli.project_trace_out.as_deref(),
                error.downcast_ref::<ProjectExecutionError>(),
            ) {
                if let Err(trace_error) = write_project_attempt_trace(path, &project_error.trace) {
                    return Err(error.context(format!(
                        "additionally failed to retain the Project HGraph attempt trace: {trace_error:#}"
                    )));
                }
            }
            return Err(error);
        }
    };

    if let Some(path) = cli.project_trace_out.as_deref() {
        let trace = execution
            .trace
            .as_ref()
            .context("HGraph project execution returned no Project HGraph attempt trace")?;
        write_project_attempt_trace(path, trace)?;
    }
    let results = execution.results;

    for result in &results {
        print!("{}", result.summary());
    }
    if !results.iter().any(|r| r.succeeded()) {
        bail!("no route succeeded");
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Input collection
// ─────────────────────────────────────────────────────────────────────────────

/// Expand the input list and compute the root against which marker paths are
/// written. Explicit files fail on invalid input. Directory walks record every
/// skipped path so callers can report exclusions uniformly.
#[cfg(test)]
fn collect_files(
    inputs: &[PathBuf],
    _ext_map: &BTreeMap<String, String>,
    exclude: Option<&Path>,
) -> Result<CollectedFiles> {
    collect_files_with_skip_mode(inputs, _ext_map, exclude, false)
}

fn collect_files_with_skip_mode(
    inputs: &[PathBuf],
    _ext_map: &BTreeMap<String, String>,
    exclude: Option<&Path>,
    enumerate_excluded_trees: bool,
) -> Result<CollectedFiles> {
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();

    for input in inputs {
        let input = absolute_path(input)?;
        if input.is_dir() {
            let mut ignore_rules = Vec::new();
            let mut state = WalkState {
                exclude,
                seen_files: &mut seen_files,
                seen_dirs: &mut seen_dirs,
                files: &mut files,
                skipped: &mut skipped,
                ignore_rules: &mut ignore_rules,
                enumerate_excluded_trees,
            };
            walk_dir(&input, &mut state)?;
        } else if input.is_file() {
            if push_unique(&input, exclude, &mut seen_files, &mut files, &mut skipped)? {
                // Explicitly-listed files must be readable text: fail loudly
                // here instead of skipping silently like directory walks do.
                fs::read_to_string(&input)
                    .with_context(|| format!("{}: not readable as UTF-8 text", input.display()))?;
            }
        } else {
            bail!("{}: no such file or directory", input.display());
        }
    }

    let marker_root = compute_marker_root(inputs, &files)?;
    Ok(CollectedFiles {
        files,
        marker_root,
        skipped,
    })
}

/// Push `path` onto `files` unless it is the excluded output file or has
/// already been collected (compared by canonical path, so symlinks and
/// `./a.py` vs `a.py` spellings dedupe correctly). Returns true if pushed.
fn push_unique(
    path: &Path,
    exclude: Option<&Path>,
    seen: &mut HashSet<PathBuf>,
    files: &mut Vec<PathBuf>,
    skipped: &mut Vec<SkippedPath>,
) -> Result<bool> {
    let canonical = path_identity(path)?;
    if exclude.is_some_and(|e| e == canonical) {
        skipped.push(SkippedPath {
            path: path.to_path_buf(),
            reason: "output file".into(),
        });
        return Ok(false);
    }
    if !seen.insert(canonical) {
        skipped.push(SkippedPath {
            path: path.to_path_buf(),
            reason: "duplicate or symlink alias".into(),
        });
        return Ok(false);
    }
    files.push(path.to_path_buf());
    Ok(true)
}

const SKIP_DIRS: &[&str] = &["target", "node_modules", "__pycache__", ".git"];

fn walk_dir(dir: &Path, state: &mut WalkState<'_>) -> Result<()> {
    // Symlink-loop protection: visit each real directory at most once.
    let canonical = dir
        .canonicalize()
        .with_context(|| format!("failed to resolve directory {}", dir.display()))?;
    if !state.seen_dirs.insert(canonical) {
        state.skipped.push(SkippedPath {
            path: dir.to_path_buf(),
            reason: "directory already visited through another path".into(),
        });
        return Ok(());
    }

    let previous_rule_count = state.ignore_rules.len();
    load_ignore_rules(dir, state.ignore_rules);

    let mut entries = Vec::new();
    for entry in
        fs::read_dir(dir).with_context(|| format!("failed to read directory {}", dir.display()))?
    {
        match entry {
            Ok(entry) => entries.push(entry.path()),
            Err(error) => state.skipped.push(SkippedPath {
                path: dir.join("<unreadable-entry>"),
                reason: format!("directory entry error: {error}"),
            }),
        }
    }
    entries.sort();

    for entry in entries {
        let name = entry
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        let is_dir = entry.is_dir();
        let is_file = entry.is_file();

        if let Some(source) = ignored_by(state.ignore_rules, &entry, is_dir) {
            record_excluded_tree(
                &entry,
                &format!("ignored by {}", source.display()),
                state.skipped,
                state.enumerate_excluded_trees,
            );
            continue;
        }

        if name.starts_with('.') {
            record_excluded_tree(
                &entry,
                "hidden path",
                state.skipped,
                state.enumerate_excluded_trees,
            );
            continue;
        }

        if is_dir {
            if SKIP_DIRS.contains(&name.as_ref()) {
                record_excluded_tree(
                    &entry,
                    "built-in excluded directory",
                    state.skipped,
                    state.enumerate_excluded_trees,
                );
                continue;
            }
            walk_dir(&entry, state)?;
            continue;
        }

        if !is_file {
            state.skipped.push(SkippedPath {
                path: entry,
                reason: "unsupported filesystem entry".into(),
            });
            continue;
        }

        match fs::read(&entry) {
            Ok(bytes) => match std::str::from_utf8(&bytes) {
                Ok(text) => {
                    if is_generated_olink_output(text) {
                        state.skipped.push(SkippedPath {
                            path: entry,
                            reason: "generated o-link output".into(),
                        });
                        continue;
                    }
                    let _ = push_unique(
                        &entry,
                        state.exclude,
                        state.seen_files,
                        state.files,
                        state.skipped,
                    )?;
                }
                Err(_) => state.skipped.push(SkippedPath {
                    path: entry,
                    reason: "not UTF-8 text".into(),
                }),
            },
            Err(error) => state.skipped.push(SkippedPath {
                path: entry,
                reason: format!("read error: {error}"),
            }),
        }
    }

    state.ignore_rules.truncate(previous_rule_count);
    Ok(())
}

fn is_generated_olink_output(text: &str) -> bool {
    let body = text
        .strip_prefix("#!/usr/bin/env o\r\n")
        .or_else(|| text.strip_prefix("#!/usr/bin/env o\n"))
        .unwrap_or(text);
    body.starts_with(O_LINK_GENERATED_HEADER) || body.starts_with("# Ostadix-lang lifted project")
}

fn record_excluded_tree(
    path: &Path,
    reason: &str,
    skipped: &mut Vec<SkippedPath>,
    enumerate_children: bool,
) {
    skipped.push(SkippedPath {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    });

    if !enumerate_children {
        return;
    }

    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.file_type().is_dir() {
        return;
    }

    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            skipped.push(SkippedPath {
                path: path.join("<unreadable-entry>"),
                reason: format!("excluded directory entry error: {error}"),
            });
            return;
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => paths.push(entry.path()),
            Err(error) => skipped.push(SkippedPath {
                path: path.join("<unreadable-entry>"),
                reason: format!("excluded directory entry error: {error}"),
            }),
        }
    }
    let mut entries = paths;
    entries.sort();
    for entry in entries {
        record_excluded_tree(&entry, reason, skipped, enumerate_children);
    }
}

fn load_ignore_rules(dir: &Path, rules: &mut Vec<IgnoreRules>) {
    for name in [".gitignore", ".olinkignore"] {
        let source = dir.join(name);
        if !source.is_file() {
            continue;
        }
        let mut builder = GitignoreBuilder::new(dir);
        if let Some(error) = builder.add(&source) {
            eprintln!(
                "warning: {}: some ignore rules could not be loaded ({error})",
                source.display()
            );
        }
        match builder.build() {
            Ok(matcher) => rules.push(IgnoreRules { source, matcher }),
            Err(error) => eprintln!(
                "warning: {}: ignore rules disabled ({error})",
                source.display()
            ),
        }
    }
}

fn ignored_by(rules: &[IgnoreRules], path: &Path, is_dir: bool) -> Option<PathBuf> {
    let mut ignored = None;
    for rule_set in rules {
        let matched = rule_set.matcher.matched(path, is_dir);
        if matched.is_ignore() {
            ignored = Some(rule_set.source.clone());
        } else if matched.is_whitelist() {
            ignored = None;
        }
    }
    ignored
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    Ok(normalized)
}

fn path_identity(path: &Path) -> Result<PathBuf> {
    path.canonicalize().or_else(|_| absolute_path(path))
}

fn compute_marker_root(inputs: &[PathBuf], files: &[PathBuf]) -> Result<PathBuf> {
    let mut anchors = Vec::new();
    for input in inputs {
        let absolute = absolute_path(input)?;
        if absolute.is_dir() {
            anchors.push(absolute);
        } else if let Some(parent) = absolute.parent() {
            anchors.push(parent.to_path_buf());
        }
    }
    for file in files {
        let absolute = absolute_path(file)?;
        if let Some(parent) = absolute.parent() {
            anchors.push(parent.to_path_buf());
        }
    }
    common_path_root(&anchors).context("inputs do not share a filesystem root")
}

fn common_path_root(paths: &[PathBuf]) -> Option<PathBuf> {
    let first = paths.first()?;
    let mut common: Vec<Component<'_>> = first.components().collect();
    for path in &paths[1..] {
        let components: Vec<Component<'_>> = path.components().collect();
        let keep = common
            .iter()
            .zip(&components)
            .take_while(|(left, right)| left == right)
            .count();
        common.truncate(keep);
    }
    if common.is_empty() {
        return None;
    }
    let mut root = PathBuf::new();
    for component in common {
        root.push(component.as_os_str());
    }
    Some(root)
}

fn marker_path(path: &Path, marker_root: &Path) -> Result<PathBuf> {
    let absolute = absolute_path(path)?;
    let relative = absolute.strip_prefix(marker_root).with_context(|| {
        format!(
            "{} is outside marker root {}",
            absolute.display(),
            marker_root.display()
        )
    })?;
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe marker path derived from {}", path.display());
    }
    let text = relative
        .to_str()
        .with_context(|| format!("{} is not a UTF-8 path", relative.display()))?;
    if text.contains(['\n', '\r']) {
        bail!("marker path contains a line break: {}", relative.display());
    }
    Ok(relative.to_path_buf())
}

/// Resolve a file path to its backend language. Unknown and extensionless
/// UTF-8 files use the inert `text` backend so arbitrary textual source trees
/// remain lossless. `.O` files map to the pseudo-backend "" (inline).
fn file_backend(path: &Path, ext_map: &BTreeMap<String, String>) -> String {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return "text".to_string();
    };
    if ext == "O" {
        return String::new();
    }
    ext_map
        .get(&ext.to_ascii_lowercase())
        .cloned()
        .unwrap_or_else(|| "text".to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// Linking
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct LinkedSection {
    marker: PathBuf,
    body: String,
    parallel_eligible: bool,
    /// Monotone execution wave derived from detected source dependencies.
    /// Sections in one wave may overlap; a later wave is emitted as a new
    /// top-level coordination call, so the evaluator settles the predecessor
    /// wave before starting it.
    dependency_wave: usize,
    decision: String,
}

#[cfg(test)]
fn link_files(
    files: &[PathBuf],
    marker_root: &Path,
    ext_map: &BTreeMap<String, String>,
    backends: &HashSet<String>,
) -> Result<String> {
    link_files_with_options(files, marker_root, ext_map, backends, None, false, false)
}

fn link_files_with_options(
    files: &[PathBuf],
    marker_root: &Path,
    ext_map: &BTreeMap<String, String>,
    backends: &HashSet<String>,
    parallel: Option<ParallelLinkMode>,
    parallel_required: bool,
    explain_parallel: bool,
) -> Result<String> {
    // Reorder same-language files according to their import-graph so that
    // files depended on by others always appear first.  Files of different
    // languages keep their relative order from the input list.
    let ordered = dependency_order_plan(files, ext_map);
    let registry = o_lang::ir::BackendRegistry::global();
    let mut sections = Vec::with_capacity(ordered.len());

    for ordered_file in &ordered {
        let path = &ordered_file.path;
        let backend = file_backend(path, ext_map);
        let mut content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let marker = marker_path(path, marker_root)?;

        let mut body = String::new();

        if backend.is_empty() {
            // .O source: strip a shebang line and inline verbatim.
            if content.starts_with("#!") {
                content = content
                    .find('\n')
                    .map(|nl| content[nl + 1..].to_string())
                    .unwrap_or_default();
            }
            body.push_str(&content);
        } else {
            // `[*]` is an explicit fresh-environment request, not a logical
            // actor identity. It cannot collide across aliases or with an
            // authored `.O` environment and remains placement-eligible.
            let tag = format!("{backend}[*]");
            let closer = format!(")_{tag}");
            let escaped = escape_body(&content, &closer, backends);
            body.push_str(&tag);
            body.push_str("^(\n");
            body.push_str(&escaped);
            body.push_str(&closer);
            body.push('\n');
        }

        let (parallel_eligible, decision) = match (parallel, backend.is_empty()) {
            (None, _) => (
                false,
                "sequential: --parallel was not requested".to_string(),
            ),
            (Some(_), true) => (
                false,
                "sequential: an inlined .O document may contain multiple ordered roots".to_string(),
            ),
            (Some(mode), false) => {
                let spec = registry
                    .get(&backend)
                    .expect("file backend came from the registered backend set");
                match mode {
                    ParallelLinkMode::Autonomous
                        if spec.execution != o_lang::ir::ExecutionMode::InlineAst =>
                    {
                        (
                            true,
                            "parallel: explicit autonomous fresh-environment opt-in".to_string(),
                        )
                    }
                    ParallelLinkMode::Autonomous => (
                        false,
                        "sequential: structural InlineAst backends stay coordinator-owned"
                            .to_string(),
                    ),
                    ParallelLinkMode::Verified
                        if spec.pure
                            && spec.execution == o_lang::ir::ExecutionMode::InlineValue =>
                    {
                        (
                            true,
                            "parallel: catalog-verified pure inline renderer".to_string(),
                        )
                    }
                    ParallelLinkMode::Verified => (
                        false,
                        "sequential: backend lacks a verified pure inline contract".to_string(),
                    ),
                }
            }
        };

        if explain_parallel {
            eprintln!("o-link placement: {}: {}", marker.display(), decision);
        }
        sections.push(LinkedSection {
            marker,
            body,
            parallel_eligible,
            dependency_wave: ordered_file.wave,
            decision,
        });
    }

    if parallel_required {
        let rejected = sections
            .iter()
            .filter(|section| !section.parallel_eligible)
            .map(|section| format!("{} ({})", section.marker.display(), section.decision))
            .collect::<Vec<_>>();
        if !rejected.is_empty() {
            bail!(
                "--parallel-required could not place {} section(s) in the {} lane:\n  {}",
                rejected.len(),
                parallel.expect("clap requires --parallel").label(),
                rejected.join("\n  ")
            );
        }
    }

    let mut out = String::new();
    out.push_str("# Linked by o-link: single-file .O program\n");
    let mut index = 0;
    while index < sections.len() {
        if parallel.is_some() && sections[index].parallel_eligible {
            let run_start = index;
            let dependency_wave = sections[index].dependency_wave;
            while index < sections.len()
                && sections[index].parallel_eligible
                && sections[index].dependency_wave == dependency_wave
            {
                index += 1;
            }
            out.push_str("\nautonomous(batch(\n");
            for (offset, section) in sections[run_start..index].iter().enumerate() {
                emit_linked_section(&mut out, section);
                if offset + 1 < index - run_start {
                    out.push_str(",\n");
                } else {
                    out.push('\n');
                }
            }
            out.push_str("))\n");
        } else {
            emit_linked_section(&mut out, &sections[index]);
            index += 1;
        }
    }

    Ok(out)
}

fn emit_linked_section(out: &mut String, section: &LinkedSection) {
    out.push('\n');
    out.push_str(&format!("# ── {} ──\n", section.marker.display()));
    out.push_str(SECTION_LENGTH_PREFIX);
    out.push_str(&section.body.len().to_string());
    out.push('\n');
    out.push_str(&section.body);
    if !section.body.ends_with('\n') {
        out.push('\n');
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Dependency ordering
// ─────────────────────────────────────────────────────────────────────────────

/// Reorder `files` so that within each language group, files that are imported
/// by others appear before the files that import them.  The relative order of
/// files from different language groups is preserved, and within a language
/// group the original (alphabetical) order is preserved for files that have
/// no dependency relationship with each other.
///
/// Cycles are broken conservatively: any file that participates in a cycle
/// keeps its original position relative to the other cycle members.
pub fn order_by_deps(files: &[PathBuf], ext_map: &BTreeMap<String, String>) -> Vec<PathBuf> {
    dependency_order_plan(files, ext_map)
        .into_iter()
        .map(|entry| entry.path)
        .collect()
}

#[derive(Clone, Debug)]
struct DependencyOrderedFile {
    path: PathBuf,
    wave: usize,
}

#[derive(Clone, Debug)]
struct GroupDependencyEntry {
    path: PathBuf,
    dependencies: Vec<PathBuf>,
    /// Cycles have no valid topological wave. Preserve the legacy fallback by
    /// serializing their remaining members in original order.
    force_barrier_before: bool,
}

/// Produce the stable file order together with the execution wave required by
/// every detected dependency. The wave numbers are monotone in emitted order,
/// preserving the existing cross-language interleave while allowing unrelated
/// files adjacent to a dependent to share its wave.
fn dependency_order_plan(
    files: &[PathBuf],
    ext_map: &BTreeMap<String, String>,
) -> Vec<DependencyOrderedFile> {
    // Group files by dependency language, preserving original indices so we
    // can interleave the sorted groups back correctly. This differs from the
    // output backend for C/C++ headers: .h/.hpp still render as inert text,
    // but they must be visible to C/C++ include ordering.
    let mut groups: HashMap<String, Vec<(usize, &PathBuf)>> = HashMap::new();
    for (i, path) in files.iter().enumerate() {
        groups
            .entry(dependency_group(path, ext_map))
            .or_default()
            .push((i, path));
    }

    // Sort each group by import-graph dependencies, then reassemble the full
    // list in original index order (preserving cross-language ordering).
    let mut sorted_entries: Vec<(usize, GroupDependencyEntry)> = Vec::with_capacity(files.len());

    for group in groups.values() {
        let orig_indices: Vec<usize> = group.iter().map(|(i, _)| *i).collect();
        let paths: Vec<&PathBuf> = group.iter().map(|(_, p)| *p).collect();
        let sorted_paths = dependency_plan_group(&paths, ext_map);
        // Zip the topo-sorted paths back with the original indices so the
        // interleave step uses the slot each file occupied in the input list.
        for (orig_i, entry) in orig_indices.iter().zip(sorted_paths) {
            sorted_entries.push((*orig_i, entry));
        }
    }

    sorted_entries.sort_by_key(|(i, _)| *i);

    let mut current_wave = 0usize;
    let mut emitted_any = false;
    let mut wave_by_path = HashMap::<PathBuf, usize>::with_capacity(files.len());
    let mut ordered = Vec::with_capacity(files.len());

    for (_, entry) in sorted_entries {
        let dependency_wave = entry
            .dependencies
            .iter()
            .filter_map(|dependency| wave_by_path.get(dependency).copied())
            .max()
            .map(|wave| wave.saturating_add(1))
            .unwrap_or(0);

        let mut wave = current_wave.max(dependency_wave);
        if emitted_any && entry.force_barrier_before {
            wave = wave.max(current_wave.saturating_add(1));
        }

        current_wave = wave;
        emitted_any = true;
        wave_by_path.insert(entry.path.clone(), wave);
        ordered.push(DependencyOrderedFile {
            path: entry.path,
            wave,
        });
    }

    ordered
}

fn dependency_group(path: &Path, ext_map: &BTreeMap<String, String>) -> String {
    let backend = file_backend(path, ext_map);
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    if matches!(backend.as_str(), "c" | "cpp")
        || matches!(extension.as_deref(), Some("h" | "hh" | "hpp" | "hxx"))
    {
        "c-family".to_string()
    } else {
        backend
    }
}

/// Topological plan for a single-language file group.
///
/// Builds a directed dependency graph among the files in `paths` using
/// language-specific import scanning, condenses strongly connected components,
/// then emits those components in stable topological order. Every dependency
/// outside a cycle therefore precedes the files that depend on it. Files that
/// have no dependency relationship keep their original relative order. Cycle
/// members have no valid topological order, so they keep their original order
/// and each receives a conservative execution barrier.
fn dependency_plan_group(
    paths: &[&PathBuf],
    ext_map: &BTreeMap<String, String>,
) -> Vec<GroupDependencyEntry> {
    if paths.len() <= 1 {
        return paths
            .iter()
            .map(|path| GroupDependencyEntry {
                path: (*path).clone(),
                dependencies: Vec::new(),
                force_barrier_before: false,
            })
            .collect();
    }

    // Build a stem→index map so we can resolve import names to file indices.
    // For `src/utils.py` the stem is `utils`; for `pkg/sub/helper.py` we also
    // register `sub.helper` and `pkg.sub.helper` (dotted module paths).
    let mut stem_to_idx: HashMap<String, usize> = HashMap::new();
    for (i, path) in paths.iter().enumerate() {
        let stems = module_stems(path);
        for s in stems {
            stem_to_idx.entry(s).or_insert(i);
        }
    }

    // For each file, collect the set of file indices it depends on.
    let mut deps: Vec<HashSet<usize>> = vec![HashSet::new(); paths.len()];
    for (i, path) in paths.iter().enumerate() {
        if let Ok(src) = fs::read_to_string(path) {
            for imp in imported_modules(&src, ext_map, path) {
                if let Some(&j) = stem_to_idx.get(&imp) {
                    if j != i {
                        deps[i].insert(j);
                    }
                }
            }
        }
    }

    let n = paths.len();
    let mut adjacency = Vec::with_capacity(n);
    for dependencies in &deps {
        let mut dependencies = dependencies.iter().copied().collect::<Vec<_>>();
        dependencies.sort_unstable();
        adjacency.push(dependencies);
    }

    // reverse_adjacency[j] = files that depend on j (j must come before them).
    let mut rev_adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, dependencies) in adjacency.iter().enumerate() {
        for &j in dependencies {
            rev_adj[j].push(i);
        }
    }
    for dependents in &mut rev_adj {
        dependents.sort_unstable();
    }

    // A cycle has no member-level topological order, but its SCC is one node
    // in the condensation DAG. Sorting that DAG preserves every satisfiable
    // dependency into and out of the cycle instead of treating downstream
    // consumers as arbitrary cycle leftovers.
    let mut components = strongly_connected_components(&adjacency, &rev_adj);
    for component in &mut components {
        component.sort_unstable();
    }
    let mut component_of = vec![usize::MAX; n];
    for (component_id, component) in components.iter().enumerate() {
        for &node in component {
            component_of[node] = component_id;
        }
    }

    let mut component_dependencies = vec![HashSet::<usize>::new(); components.len()];
    let mut component_dependents = vec![HashSet::<usize>::new(); components.len()];
    for (node, dependencies) in adjacency.iter().enumerate() {
        let dependent_component = component_of[node];
        for &dependency in dependencies {
            let dependency_component = component_of[dependency];
            if dependency_component != dependent_component {
                component_dependencies[dependent_component].insert(dependency_component);
                component_dependents[dependency_component].insert(dependent_component);
            }
        }
    }

    let component_min_index = components
        .iter()
        .map(|component| component[0])
        .collect::<Vec<_>>();
    let mut in_degree = component_dependencies
        .iter()
        .map(HashSet::len)
        .collect::<Vec<_>>();
    let mut ready = (0..components.len())
        .filter(|&component| in_degree[component] == 0)
        .collect::<Vec<_>>();
    let mut result = Vec::with_capacity(n);

    while !ready.is_empty() {
        // Pick the component whose first member appeared earliest. This is the
        // stable analogue of Kahn's algorithm on the condensation DAG.
        let pos = ready
            .iter()
            .enumerate()
            .min_by_key(|(_, &component)| component_min_index[component])
            .map(|(p, _)| p)
            .unwrap();
        let component = ready.remove(pos);
        let cyclic = components[component].len() > 1;
        for &node in &components[component] {
            let mut effective_dependencies = adjacency[node].clone();
            // An edge to any member of an SCC is transitively an edge from the
            // whole component. Its last emitted member is the conservative
            // completion frontier used by the global wave planner.
            effective_dependencies.extend(component_dependencies[component].iter().map(
                |&dependency_component| {
                    *components[dependency_component]
                        .last()
                        .expect("dependency components are non-empty")
                },
            ));
            effective_dependencies.sort_unstable();
            effective_dependencies.dedup();
            result.push(GroupDependencyEntry {
                path: paths[node].clone(),
                dependencies: effective_dependencies
                    .iter()
                    .map(|&dependency| paths[dependency].clone())
                    .collect(),
                force_barrier_before: cyclic,
            });
        }

        let mut dependents = component_dependents[component]
            .iter()
            .copied()
            .collect::<Vec<_>>();
        dependents.sort_by_key(|&dependent| component_min_index[dependent]);
        for dependent in dependents {
            in_degree[dependent] -= 1;
            if in_degree[dependent] == 0 {
                ready.push(dependent);
            }
        }
    }

    debug_assert_eq!(
        result.len(),
        n,
        "the SCC condensation graph must be acyclic"
    );
    result
}

/// Compute SCCs without recursive DFS so linking a very large source tree
/// cannot exhaust the process stack. Traversal order is deterministic because
/// both adjacency lists are sorted before this helper is called.
fn strongly_connected_components(
    adjacency: &[Vec<usize>],
    reverse_adjacency: &[Vec<usize>],
) -> Vec<Vec<usize>> {
    let mut visited = vec![false; adjacency.len()];
    let mut finish_order = Vec::with_capacity(adjacency.len());

    for start in 0..adjacency.len() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut stack = vec![(start, 0usize)];
        while let Some((node, next_edge)) = stack.last_mut() {
            if *next_edge < adjacency[*node].len() {
                let successor = adjacency[*node][*next_edge];
                *next_edge += 1;
                if !visited[successor] {
                    visited[successor] = true;
                    stack.push((successor, 0));
                }
            } else {
                let (finished, _) = stack.pop().expect("DFS stack is non-empty");
                finish_order.push(finished);
            }
        }
    }

    visited.fill(false);
    let mut components = Vec::new();
    for &start in finish_order.iter().rev() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut component = Vec::new();
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            component.push(node);
            for &successor in reverse_adjacency[node].iter().rev() {
                if !visited[successor] {
                    visited[successor] = true;
                    stack.push(successor);
                }
            }
        }
        components.push(component);
    }
    components
}

/// Return the set of module-name stems that `path` could be imported as.
///
/// For `/some/src/pkg/utils.py`, we return `["utils", "pkg.utils"]`.
/// We stop at directory components named `src`, `lib`, or `source` since
/// those are common source roots that are not part of the import path.
fn module_stems(path: &Path) -> Vec<String> {
    let mut stems = Vec::new();
    let stem = match path.file_stem().and_then(|s| s.to_str()) {
        Some(s) if s != "__init__" => s.to_string(),
        _ => return stems,
    };

    stems.push(stem.clone());

    // Build dotted-path variants by walking parent components.
    let mut parts: Vec<String> = vec![stem];
    let root_markers = ["src", "lib", "source", "tests"];
    for component in path
        .parent()
        .map(|p| p.components())
        .into_iter()
        .flatten()
        .rev()
    {
        let name = match component {
            std::path::Component::Normal(n) => n.to_str().unwrap_or("").to_string(),
            _ => break,
        };
        if root_markers.contains(&name.as_str()) || name.starts_with('.') {
            break;
        }
        parts.insert(0, name.clone());
        stems.push(parts.join("."));
    }

    stems
}

/// Extract module names imported by one source file. This is intentionally a
/// lightweight dependency scanner, not a full parser. It recognizes the
/// ordinary static import forms of each hosted language and returns module
/// stems suitable for lookup in `stem_to_idx`.
fn imported_modules(src: &str, ext_map: &BTreeMap<String, String>, path: &Path) -> Vec<String> {
    let lang = file_backend(path, ext_map);
    let mut mods = Vec::new();

    for line in src.lines() {
        let line = line.trim();
        match lang.as_str() {
            "python" => {
                // `import X`, `import X as Y`, `import X, Y`
                if let Some(rest) = line.strip_prefix("import ") {
                    for part in rest.split(',') {
                        push_import_candidates(
                            &mut mods,
                            part.split_whitespace().next().unwrap_or(""),
                        );
                    }
                }
                // `from X import Y`: the dependency is X.
                if let Some(rest) = line.strip_prefix("from ") {
                    let module = rest.split_whitespace().next().unwrap_or("");
                    push_import_candidates(&mut mods, module);
                }
            }
            "javascript" => {
                if line.starts_with("import ") || line.starts_with("export ") {
                    if let Some(specifier) = quoted_text(line) {
                        push_import_candidates(&mut mods, specifier);
                    }
                }
                for prefix in ["require(", "import("] {
                    if let Some(start) = line.find(prefix) {
                        if let Some(specifier) = quoted_text(&line[start + prefix.len()..]) {
                            push_import_candidates(&mut mods, specifier);
                        }
                    }
                }
            }
            "rust" => {
                let line = line.strip_prefix("pub ").unwrap_or(line);
                if let Some(module) = line.strip_prefix("mod ") {
                    push_import_candidates(&mut mods, module.trim_end_matches(';'));
                }
                if let Some(module) = line.strip_prefix("use ") {
                    push_import_candidates(&mut mods, module.trim_end_matches(';'));
                }
                if let Some(module) = line.strip_prefix("extern crate ") {
                    push_import_candidates(&mut mods, module.trim_end_matches(';'));
                }
            }
            "c" | "cpp" => {
                if let Some(include) = line.strip_prefix("#include") {
                    if include.trim_start().starts_with('"') {
                        if let Some(specifier) = quoted_text(include) {
                            push_import_candidates(&mut mods, specifier);
                        }
                    }
                }
            }
            "java" => {
                if let Some(module) = line.strip_prefix("import ") {
                    let module = module.strip_prefix("static ").unwrap_or(module);
                    push_import_candidates(
                        &mut mods,
                        module.trim_end_matches(';').trim_end_matches(".*"),
                    );
                }
            }
            "haskell" => {
                if let Some(module) = line.strip_prefix("import ") {
                    let module = module.strip_prefix("qualified ").unwrap_or(module);
                    push_import_candidates(
                        &mut mods,
                        module.split_whitespace().next().unwrap_or(""),
                    );
                }
            }
            "ruby" => {
                if line.starts_with("require ") || line.starts_with("require_relative ") {
                    if let Some(specifier) = quoted_text(line) {
                        push_import_candidates(&mut mods, specifier);
                    }
                }
            }
            "ocaml" => {
                for prefix in ["open ", "include "] {
                    if let Some(module) = line.strip_prefix(prefix) {
                        push_import_candidates(
                            &mut mods,
                            module.split_whitespace().next().unwrap_or(""),
                        );
                    }
                }
            }
            "racket" | "lisp" | "common_lisp" => {
                if line.contains("require") || line.contains("load") {
                    if let Some(specifier) = quoted_text(line) {
                        push_import_candidates(&mut mods, specifier);
                    }
                }
            }
            "bash" | "shell" => {
                for prefix in ["source ", ". "] {
                    if let Some(specifier) = line.strip_prefix(prefix) {
                        push_import_candidates(
                            &mut mods,
                            specifier.split_whitespace().next().unwrap_or(""),
                        );
                    }
                }
            }
            "nix" | "nix_expr" => {
                for token in line.split(|ch: char| {
                    ch.is_whitespace() || matches!(ch, '(' | ')' | '{' | '}' | '[' | ']' | ';')
                }) {
                    if token.starts_with("./") || token.starts_with("../") {
                        push_import_candidates(&mut mods, token);
                    }
                }
            }
            "csharp" => {
                if let Some(module) = line.strip_prefix("using ") {
                    push_import_candidates(&mut mods, module.trim_end_matches(';'));
                }
            }
            "mathematica" | "matlab" => {
                if let Some(specifier) = quoted_text(line) {
                    if line.contains("Get")
                        || line.contains("Needs")
                        || line.contains("run(")
                        || line.contains("source(")
                    {
                        push_import_candidates(&mut mods, specifier);
                    }
                }
            }
            _ => {}
        }
    }

    mods.sort();
    mods.dedup();
    mods
}

fn quoted_text(text: &str) -> Option<&str> {
    let (start, quote) = text
        .char_indices()
        .find(|(_, ch)| matches!(ch, '\'' | '"'))?;
    let rest = &text[start + quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(&rest[..end])
}

fn push_import_candidates(modules: &mut Vec<String>, specifier: &str) {
    let mut specifier = specifier
        .trim()
        .trim_matches(|ch: char| matches!(ch, '\'' | '"' | '(' | ')' | ';' | ','))
        .trim_start_matches("crate::")
        .trim_start_matches("self::")
        .trim_start_matches("super::")
        .trim_start_matches("./")
        .trim_start_matches("../")
        .replace("::", ".")
        .replace(['/', '\\'], ".");
    for extension in [
        ".py", ".js", ".mjs", ".cjs", ".rs", ".h", ".hpp", ".c", ".cpp", ".java", ".hs", ".rb",
        ".ml", ".rkt", ".scm", ".lisp", ".sh", ".nix", ".wl", ".m",
    ] {
        if specifier.ends_with(extension) {
            specifier.truncate(specifier.len() - extension.len());
            break;
        }
    }
    let specifier = specifier
        .trim_matches('.')
        .trim_end_matches(".*")
        .trim_end_matches("::{")
        .trim();
    if specifier.is_empty() {
        return;
    }
    modules.push(specifier.to_string());
    if let Some(first) = specifier.split('.').next() {
        modules.push(first.to_string());
    }
    if let Some(last) = specifier.rsplit('.').next() {
        modules.push(last.to_string());
        let lowercase = last.to_ascii_lowercase();
        if lowercase != last {
            modules.push(lowercase);
        }
    }
}

/// Backslash-escape any text in `body` that the Ostadix-lang parser would otherwise
/// treat as syntax inside a `wrapper^( ... )_wrapper` block:
///
///   * any registered opener `IDENT[N]?{attr}?^(`  →  `\IDENT...^(`
///   * the wrapping block's exact closer `)_wrapper`  →  `\)_wrapper`
///   * any splice `$IDENT`                          →  `\$IDENT`
///
/// The parser consumes the backslash and emits the literal text, so the
/// backend receives the file contents unchanged.
fn escape_body(body: &str, closer: &str, backends: &HashSet<String>) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;

    while i < bytes.len() {
        if exact_closer_len_at(body, i, closer).is_some() {
            out.push('\\');
            out.push_str(closer);
            i += closer.len();
            continue;
        }
        if let Some(len) = opener_len(&body[i..], backends) {
            out.push('\\');
            out.push_str(&body[i..i + len]);
            i += len;
            continue;
        }
        // Escape `$IDENT`. The Ostadix-lang parser treats `$name` as a splice
        // (variable reference). Backslash-escaping it (`\$name`) makes the
        // parser emit the literal text `$name`, so the backend receives the
        // original file contents unchanged. This is critical for shell
        // scripts (`$HOME`, `$PATH`, …) and any language that uses `$`
        // followed by an identifier-shaped name.
        if bytes[i] == b'$'
            && i + 1 < bytes.len()
            && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_')
        {
            out.push('\\');
            out.push('$');
            i += 1;
            continue;
        }
        // Advance one full UTF-8 character.
        let ch = body[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

/// Return the closer length only when `closer` is a complete lexical tag at
/// `position`. This intentionally mirrors `Parser::exact_closer_len_at`:
///
/// * a bare tag can still grow an identifier, environment, or attribute;
/// * an environment tag can still grow an attribute;
/// * an attributed tag is already complete.
///
/// Prefix-only escaping is not lossless. For example, escaping the prefix in
/// `)_python[*]{defer}` would leave the backslash intact because the parser
/// correctly sees that prefix as a non-closer.
fn exact_closer_len_at(source: &str, position: usize, closer: &str) -> Option<usize> {
    let raw_tag = closer.strip_prefix(")_")?;
    let remaining = source.get(position..)?;
    if !remaining.starts_with(closer) {
        return None;
    }

    let next = remaining.as_bytes().get(closer.len()).copied();
    let has_attributes = raw_tag.as_bytes().contains(&b'{');
    let tag_before_attributes = raw_tag.split('{').next().unwrap_or(raw_tag);
    let has_environment = tag_before_attributes.as_bytes().contains(&b'[');
    let extends_tag = if has_attributes {
        false
    } else if has_environment {
        next == Some(b'{')
    } else {
        next.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || matches!(next, Some(b'[' | b'{'))
    };
    (!extends_tag).then_some(closer.len())
}

/// If `s` begins with a registered opener (`IDENT [N|*]? {attr}? ^(`), return
/// the byte length of the opener text including the trailing `^(`.
fn opener_len(s: &str, backends: &HashSet<String>) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if !backends.contains(&s[..i]) {
        return None;
    }
    // Optional numeric persistent or `[*]` linker-isolated env marker.
    if i < bytes.len() && bytes[i] == b'[' {
        let mut j = i + 1;
        if j < bytes.len() && bytes[j] == b'*' {
            j += 1;
        } else {
            let digits_start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j == digits_start {
                return None;
            }
        }
        if j < bytes.len() && bytes[j] == b']' {
            i = j + 1;
        }
    }
    // Optional `{attr}` marker.
    if i < bytes.len() && bytes[i] == b'{' {
        let mut j = i + 1;
        while j < bytes.len() && bytes[j] != b'}' {
            j += 1;
        }
        if j > i + 1 && j < bytes.len() && bytes[j] == b'}' {
            i = j + 1;
        }
    }
    if s[i..].starts_with("^(") {
        Some(i + 2)
    } else {
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tables
// ─────────────────────────────────────────────────────────────────────────────

/// Built-in extension → backend mapping. Keys are lowercase extensions
/// without the leading dot. `.O` is handled separately (inline).
fn default_extension_map() -> BTreeMap<String, String> {
    [
        ("py", "python"),
        ("sh", "bash"),
        ("bash", "bash"),
        ("html", "html"),
        ("htm", "html"),
        ("tex", "latex"),
        ("md", "markdown"),
        ("markdown", "markdown"),
        ("rs", "rust"),
        ("rkt", "racket"),
        ("nix", "nix"),
        ("txt", "text"),
        ("cs", "csharp"),
        ("c", "c"),
        ("cc", "cpp"),
        ("cpp", "cpp"),
        ("cxx", "cpp"),
        ("h", "text"),
        ("hpp", "text"),
        ("hs", "haskell"),
        ("lisp", "lisp"),
        ("cl", "common_lisp"),
        ("sql", "sql"),
        ("rb", "ruby"),
        ("m", "matlab"),
        ("wl", "mathematica"),
        ("wat", "webassembly"),
        ("java", "java"),
        ("js", "javascript"),
        ("mjs", "javascript"),
        ("cjs", "javascript"),
        ("ml", "ocaml"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// The registered backend set must stay in sync with `registered_backends`
/// in src/main.rs so o-link escapes exactly the openers the runtime parses.
fn registered_backends() -> HashSet<String> {
    // Single source of truth: the central BackendRegistry owns the set of
    // accepted parser tags (canonical names plus aliases).
    o_lang::ir::BackendRegistry::global().registered_backend_tags()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_trace_cli_requires_run_and_accepts_a_path() {
        let cli = Cli::try_parse_from([
            "o-link",
            "project",
            "--run",
            "--project-trace-out",
            "attempt.json",
        ])
        .unwrap();
        assert_eq!(
            cli.project_trace_out.as_deref(),
            Some(Path::new("attempt.json"))
        );

        let error =
            Cli::try_parse_from(["o-link", "project", "--project-trace-out", "attempt.json"])
                .unwrap_err();
        assert!(error.to_string().contains("--run"));
    }

    #[test]
    fn mesh_cli_defaults_to_prefer_and_builds_exact_config() {
        use o_lang::hosted_remote::project_mesh::{MeshLocalFallback, MeshRequirement};

        // `require_equals` prevents the optional mesh mode from swallowing a
        // positional project path when the flag appears first.
        let cli = Cli::try_parse_from(["o-link", "--mesh", "--run", "project"]).unwrap();
        assert_eq!(cli.mesh, Some(MeshMode::Prefer));
        assert!(has_project_intent(&cli));

        let config = mesh_execution_config(&cli).expect("--mesh must construct mesh config");
        assert_eq!(config.requirement, MeshRequirement::Prefer);
        assert_eq!(config.max_retries, 2);
        assert_eq!(config.local_fallback, MeshLocalFallback::PreSend);
        assert!(config.discover_lan);
        assert_eq!(
            config.discovery_timeout,
            std::time::Duration::from_millis(750)
        );
        assert_eq!(config.peer_root, None);
        assert_eq!(config.trace_out, None);
        assert!(!config.explain);
    }

    #[test]
    fn mesh_cli_maps_required_mode_and_all_overrides() {
        use o_lang::hosted_remote::project_mesh::{MeshLocalFallback, MeshRequirement};

        let cli = Cli::try_parse_from([
            "o-link",
            "project",
            "--run",
            "--mesh=required",
            "--mesh-retries=5",
            "--mesh-local-fallback=idempotent",
            "--mesh-discovery-timeout-ms=1250",
            "--mesh-no-lan-discovery",
            "--mesh-peer-root=peer-state",
            "--mesh-trace-out=mesh-attempt.json",
            "--explain-mesh",
        ])
        .unwrap();

        let config = mesh_execution_config(&cli).expect("--mesh must construct mesh config");
        assert_eq!(config.requirement, MeshRequirement::Required);
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.local_fallback, MeshLocalFallback::Idempotent);
        assert!(!config.discover_lan);
        assert_eq!(
            config.discovery_timeout,
            std::time::Duration::from_millis(1_250)
        );
        assert_eq!(config.peer_root.as_deref(), Some(Path::new("peer-state")));
        assert_eq!(
            config.trace_out.as_deref(),
            Some(Path::new("mesh-attempt.json"))
        );
        assert!(config.explain);
    }

    #[test]
    fn mesh_cli_requires_run_and_tuning_flags_require_mesh() {
        let missing_run = Cli::try_parse_from(["o-link", "project", "--mesh"]).unwrap_err();
        assert!(missing_run.to_string().contains("--run"));

        for args in [
            vec!["o-link", "project", "--run", "--mesh-retries=3"],
            vec!["o-link", "project", "--run", "--mesh-local-fallback=never"],
            vec![
                "o-link",
                "project",
                "--run",
                "--mesh-discovery-timeout-ms=900",
            ],
            vec!["o-link", "project", "--run", "--mesh-no-lan-discovery"],
            vec!["o-link", "project", "--run", "--mesh-peer-root=peers"],
            vec!["o-link", "project", "--run", "--mesh-trace-out=trace.json"],
            vec!["o-link", "project", "--run", "--explain-mesh"],
        ] {
            let error = Cli::try_parse_from(args).unwrap_err();
            assert!(
                error.to_string().contains("--mesh"),
                "unexpected clap error: {error}"
            );
        }

        let zero_timeout = Cli::try_parse_from([
            "o-link",
            "project",
            "--run",
            "--mesh",
            "--mesh-discovery-timeout-ms=0",
        ])
        .unwrap_err();
        assert!(zero_timeout
            .to_string()
            .contains("mesh-discovery-timeout-ms"));

        let too_many_retries =
            Cli::try_parse_from(["o-link", "project", "--run", "--mesh", "--mesh-retries=65"])
                .unwrap_err();
        assert!(too_many_retries.to_string().contains("mesh-retries"));

        let too_long_discovery = Cli::try_parse_from([
            "o-link",
            "project",
            "--run",
            "--mesh",
            "--mesh-discovery-timeout-ms=60001",
        ])
        .unwrap_err();
        assert!(too_long_discovery
            .to_string()
            .contains("mesh-discovery-timeout-ms"));
    }

    #[test]
    fn mesh_is_project_only_while_parallel_remains_literal_only() {
        let conflict =
            Cli::try_parse_from(["o-link", "project", "--literal", "--run", "--mesh"]).unwrap_err();
        assert!(conflict.to_string().contains("--literal"));
        assert!(conflict.to_string().contains("--mesh"));

        let trace_conflict = Cli::try_parse_from([
            "o-link",
            "project",
            "--run",
            "--mesh",
            "--project-trace-out=project-attempt.json",
        ])
        .unwrap_err();
        assert!(trace_conflict.to_string().contains("--mesh"));
        assert!(trace_conflict.to_string().contains("--project-trace-out"));

        let mesh_parallel =
            Cli::try_parse_from(["o-link", "project", "--run", "--mesh", "--parallel"]).unwrap();
        let error = ensure_project_compatible_flags(&mesh_parallel).unwrap_err();
        assert!(error.to_string().contains("--parallel"));

        let literal_parallel = Cli::try_parse_from(["o-link", "script.py", "--parallel"]).unwrap();
        assert!(!has_project_intent(&literal_parallel));
        assert!(mesh_execution_config(&literal_parallel).is_none());
    }
    use o_lang::parser::{reconstruct_source, ONode, Parser};

    fn parse(src: &str) -> Vec<ONode> {
        let backends = registered_backends();
        Parser::new(src, &backends).parse().unwrap()
    }

    /// Concatenate all raw text inside the body of the first TypedExpr.
    fn first_block_text(nodes: &[ONode]) -> String {
        for node in nodes {
            if let ONode::TypedExpr { body, .. } = node {
                return reconstruct_source(body);
            }
        }
        panic!("no TypedExpr in parsed output");
    }

    #[test]
    fn escape_is_identity_for_plain_code() {
        let backends = registered_backends();
        let src = "x = 1 + 2\nprint(x)\n";
        assert_eq!(escape_body(src, ")_python", &backends), src);
    }

    #[test]
    fn escapes_opener_and_closer_collisions() {
        let backends = registered_backends();
        let src = "s = \"python^(1)_python\"";
        let escaped = escape_body(src, ")_python", &backends);
        assert_eq!(escaped, "s = \"\\python^(1\\)_python\"");
    }

    #[test]
    fn escaped_body_round_trips_through_parser() {
        let backends = registered_backends();
        let inner = "doc = \"use python^( ... )_python blocks\"\nx = 2 ^ (3 + 1)\n";
        let escaped = escape_body(inner, ")_python", &backends);
        let combined = format!("python^(\n{})_python\n", escaped);
        let nodes = parse(&combined);
        let body = first_block_text(&nodes);
        assert_eq!(body.trim_start_matches('\n'), inner);
    }

    #[test]
    fn indexed_closer_escaping_is_exact() {
        let backends = registered_backends();
        let inner = ")_python stays literal; )_python[0] is the real closer";
        let escaped = escape_body(inner, ")_python[0]", &backends);
        assert!(escaped.starts_with(")_python stays literal"));
        assert!(escaped.contains("\\)_python[0]"));
        let combined = format!("python[0]^(\n{escaped})_python[0]\n");
        let nodes = parse(&combined);
        let body = first_block_text(&nodes);
        assert_eq!(body.trim_start_matches('\n'), inner);
    }

    #[test]
    fn environment_closer_attribute_prefix_is_not_escaped_and_round_trips() {
        let backends = registered_backends();
        let inner = "value = ')_python[*]{defer}'\n";
        let escaped = escape_body(inner, ")_python[*]", &backends);
        assert_eq!(escaped, inner, "a longer tag prefix is literal text");

        let combined = format!("python[*]^(\n{escaped})_python[*]\n");
        let nodes = parse(&combined);
        let body = first_block_text(&nodes);
        assert_eq!(body.trim_start_matches('\n'), inner);
    }

    #[test]
    fn bare_closer_identifier_continuations_are_not_escaped_and_round_trip() {
        let backends = registered_backends();
        let inner = ")_pythonista )_python2 )_python_suffix\n";
        let escaped = escape_body(inner, ")_python", &backends);
        assert_eq!(escaped, inner, "identifier continuations extend a bare tag");

        let combined = format!("python^(\n{escaped})_python\n");
        let nodes = parse(&combined);
        let body = first_block_text(&nodes);
        assert_eq!(body.trim_start_matches('\n'), inner);
    }

    #[test]
    fn environment_closer_followed_by_identifier_is_exact_and_round_trips() {
        let backends = registered_backends();
        let inner = ")_python[*]tail\n";
        let escaped = escape_body(inner, ")_python[*]", &backends);
        assert_eq!(escaped, "\\)_python[*]tail\n");

        let combined = format!("python[*]^(\n{escaped})_python[*]\n");
        let nodes = parse(&combined);
        let body = first_block_text(&nodes);
        assert_eq!(body.trim_start_matches('\n'), inner);
    }

    #[test]
    fn foreign_closers_are_left_alone() {
        let backends = registered_backends();
        let src = "html closer: )_html stays literal";
        assert_eq!(escape_body(src, ")_python", &backends), src);
    }

    #[test]
    fn env_and_attr_openers_are_escaped() {
        let backends = registered_backends();
        let src = "python[3]^(x)_python[3] and python{lazy}^(y)_python{lazy}";
        let escaped = escape_body(src, ")_bash", &backends);
        assert!(escaped.contains("\\python[3]^("));
        assert!(escaped.contains("\\python{lazy}^("));
    }

    #[test]
    fn unregistered_idents_are_not_escaped() {
        let backends = registered_backends();
        let src = "result = pow2^(n) if weird else 2 ^ (x+1)";
        assert_eq!(escape_body(src, ")_python", &backends), src);
    }

    #[test]
    fn dollar_ident_splices_are_escaped() {
        let backends = registered_backends();
        let src = "echo $HOME and $PATH";
        let escaped = escape_body(src, ")_bash", &backends);
        assert_eq!(escaped, "echo \\$HOME and \\$PATH");
    }

    #[test]
    fn dollar_non_ident_is_left_alone() {
        let backends = registered_backends();
        // The parser does not treat $1, $@, or $? as splices, so they need no escaping.
        let src = "echo $1 $@ $? $$";
        assert_eq!(escape_body(src, ")_bash", &backends), src);
    }

    #[test]
    fn dollar_ident_round_trips_through_parser() {
        let backends = registered_backends();
        let inner = "echo $HOME\ncd $PATH/bin\n";
        let escaped = escape_body(inner, ")_bash[0]", &backends);
        assert!(escaped.contains("\\$HOME"));
        assert!(escaped.contains("\\$PATH"));
        // Use [0] env_id syntax to exercise the same delimiter shape as link_files.
        let combined = format!("bash[0]^(\n{})_bash[0]\n", escaped);
        let nodes = parse(&combined);
        let body = first_block_text(&nodes);
        assert_eq!(body.trim_start_matches('\n'), inner);
    }

    #[test]
    fn default_map_covers_common_scripts() {
        let map = default_extension_map();
        assert_eq!(map.get("py").unwrap(), "python");
        assert_eq!(map.get("sh").unwrap(), "bash");
        assert_eq!(map.get("html").unwrap(), "html");
        assert_eq!(map.get("md").unwrap(), "markdown");
    }

    #[test]
    fn unknown_and_extensionless_text_use_inert_text_backend() {
        let map = default_extension_map();
        assert_eq!(file_backend(Path::new("component.svelte"), &map), "text");
        assert_eq!(file_backend(Path::new("module.ts"), &map), "text");
        assert_eq!(file_backend(Path::new("README"), &map), "text");
        assert_eq!(file_backend(Path::new("program.O"), &map), "");
    }

    #[test]
    fn skip_report_is_aggregated_unless_verbose() {
        let collection = CollectedFiles {
            files: vec![PathBuf::from("selected.py")],
            marker_root: PathBuf::from("."),
            skipped: vec![
                SkippedPath {
                    path: PathBuf::from("one.bin"),
                    reason: "not UTF-8 text".into(),
                },
                SkippedPath {
                    path: PathBuf::from("two.bin"),
                    reason: "not UTF-8 text".into(),
                },
                SkippedPath {
                    path: PathBuf::from(".hidden"),
                    reason: "hidden path".into(),
                },
            ],
        };

        assert_eq!(
            collection.report_lines(false),
            vec![
                "warning: skipped 1 path (hidden path)",
                "warning: skipped 2 paths (not UTF-8 text)",
                "o-link scan: 1 selected, 3 skipped",
            ]
        );
        let verbose = collection.report_lines(true);
        assert_eq!(verbose.len(), 4);
        assert!(verbose[0].contains("one.bin"));
        assert!(verbose[1].contains("two.bin"));
        assert!(verbose[2].contains(".hidden"));
    }

    /// Build a unique scratch directory for filesystem-backed tests.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("olink_test_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn collect_dedupes_overlapping_inputs() {
        let dir = scratch("dedupe");
        let file = dir.join("a.py");
        fs::write(&file, "x = 1\n").unwrap();

        let map = default_extension_map();
        // Same file via the directory AND explicitly: linked once.
        let collection = collect_files(&[dir.clone(), file.clone()], &map, None).unwrap();
        assert_eq!(collection.files.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_excludes_output_and_binary_files() {
        let dir = scratch("exclude");
        fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        fs::write(dir.join("out.O"), "stale combined output\n").unwrap();
        fs::write(dir.join("blob.py"), [0xff_u8, 0xfe, 0x00]).unwrap();

        let map = default_extension_map();
        let exclude = dir.join("out.O").canonicalize().unwrap();
        let collection = collect_files(std::slice::from_ref(&dir), &map, Some(&exclude)).unwrap();

        // Only a.py: out.O is the excluded output, blob.py is not UTF-8.
        assert_eq!(collection.files.len(), 1);
        assert!(collection.files[0].ends_with("a.py"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_walk_skips_generated_olink_outputs() {
        let dir = scratch("skip_generated_olink");
        fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        fs::write(dir.join("ordinary.O"), "python^(1)_python\n").unwrap();
        fs::write(
            dir.join("combined.O"),
            "# Linked by o-link: single-file .O program\npython^(2)_python\n",
        )
        .unwrap();
        fs::write(
            dir.join("project.O"),
            "# Ostadix-lang lifted project\n# generated bundle\n",
        )
        .unwrap();
        fs::write(
            dir.join("executable.O"),
            "#!/usr/bin/env o\n# Linked by o-link: generated\n",
        )
        .unwrap();

        let map = default_extension_map();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();

        assert_eq!(collection.files.len(), 2);
        assert!(collection.files.iter().any(|path| path.ends_with("a.py")));
        assert!(collection
            .files
            .iter()
            .any(|path| path.ends_with("ordinary.O")));
        for generated in ["combined.O", "project.O", "executable.O"] {
            assert!(!collection
                .files
                .iter()
                .any(|path| path.ends_with(generated)));
            assert!(collection
                .skipped
                .iter()
                .any(|skip| skip.path.ends_with(generated)
                    && skip.reason == "generated o-link output"));
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn marker_paths_are_relative_to_the_input_root() {
        let dir = scratch("relative_markers");
        let project = dir.join("project");
        let nested = project.join("src/nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("main.py"), "print('ok')").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&project), &map, None).unwrap();
        assert_eq!(collection.marker_root, absolute_path(&project).unwrap());
        let combined =
            link_files(&collection.files, &collection.marker_root, &map, &backends).unwrap();

        assert!(combined.contains("# ── src/nested/main.py ──"));
        assert!(!combined.contains(&project.display().to_string()));
        assert!(combined.contains(SECTION_LENGTH_PREFIX));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn marker_paths_use_the_common_root_of_multiple_inputs() {
        let dir = scratch("common_marker_root");
        let left = dir.join("left/src");
        let right = dir.join("right/lib");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        fs::write(left.join("main.py"), "print('left')\n").unwrap();
        fs::write(right.join("util.py"), "print('right')\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(&[left.clone(), right.clone()], &map, None).unwrap();
        assert_eq!(collection.marker_root, dir);
        let combined =
            link_files(&collection.files, &collection.marker_root, &map, &backends).unwrap();
        assert!(combined.contains("# ── left/src/main.py ──"));
        assert!(combined.contains("# ── right/lib/util.py ──"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_walk_keeps_unknown_text_and_records_real_skips() {
        let dir = scratch("skip_report");
        fs::create_dir_all(dir.join("cache")).unwrap();
        fs::write(dir.join(".gitignore"), "*.py\n!keep.py\ncache/\n").unwrap();
        fs::write(dir.join(".olinkignore"), "notes.txt\n").unwrap();
        fs::write(dir.join("keep.py"), "print('keep')\n").unwrap();
        fs::write(dir.join("ignored.py"), "print('ignored')\n").unwrap();
        fs::write(dir.join("notes.txt"), "ignored by o-link\n").unwrap();
        fs::write(dir.join("README"), "extensionless\n").unwrap();
        fs::write(dir.join("unknown.xyz"), "unknown\n").unwrap();
        fs::write(dir.join("binary.rs"), [0xff_u8, 0x00]).unwrap();
        fs::write(dir.join(".hidden.py"), "hidden\n").unwrap();
        fs::write(dir.join("cache/generated.py"), "cached\n").unwrap();

        let map = default_extension_map();
        let collection =
            collect_files_with_skip_mode(std::slice::from_ref(&dir), &map, None, true).unwrap();
        assert_eq!(collection.files.len(), 3);
        assert!(collection
            .files
            .iter()
            .any(|path| path.ends_with("keep.py")));
        assert!(collection.files.iter().any(|path| path.ends_with("README")));
        assert!(collection
            .files
            .iter()
            .any(|path| path.ends_with("unknown.xyz")));

        let reasons = collection
            .skipped
            .iter()
            .map(|skip| skip.reason.as_str())
            .collect::<Vec<_>>();
        assert!(reasons.iter().any(|reason| reason.contains(".gitignore")));
        assert!(reasons.iter().any(|reason| reason.contains(".olinkignore")));
        assert!(reasons.contains(&"not UTF-8 text"));
        assert!(reasons.contains(&"hidden path"));
        assert!(collection
            .skipped
            .iter()
            .any(|skip| skip.path.ends_with("cache/generated.py")));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_walk_records_excluded_subtrees_without_descending() {
        let dir = scratch("bounded_skips");
        fs::create_dir_all(dir.join(".hidden/deep")).unwrap();
        fs::create_dir_all(dir.join("cache/deep")).unwrap();
        fs::write(dir.join(".gitignore"), "cache/\n").unwrap();
        fs::write(dir.join("keep.py"), "print('keep')\n").unwrap();
        fs::write(dir.join(".hidden/deep/a.py"), "print('hidden')\n").unwrap();
        fs::write(dir.join("cache/deep/generated.py"), "print('cache')\n").unwrap();

        let map = default_extension_map();
        let default_collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        assert_eq!(default_collection.files.len(), 1);
        assert_eq!(
            default_collection
                .skipped
                .iter()
                .filter(|skip| skip.path.ends_with(".hidden"))
                .count(),
            1
        );
        assert_eq!(
            default_collection
                .skipped
                .iter()
                .filter(|skip| skip.reason.contains(".gitignore"))
                .count(),
            1
        );
        assert!(!default_collection
            .skipped
            .iter()
            .any(|skip| skip.path.ends_with(".hidden/deep/a.py")));
        assert!(!default_collection
            .skipped
            .iter()
            .any(|skip| skip.path.ends_with("cache/deep/generated.py")));

        let verbose_collection =
            collect_files_with_skip_mode(std::slice::from_ref(&dir), &map, None, true).unwrap();
        assert!(verbose_collection
            .skipped
            .iter()
            .any(|skip| skip.path.ends_with(".hidden/deep/a.py")));
        assert!(verbose_collection
            .skipped
            .iter()
            .any(|skip| skip.path.ends_with("cache/deep/generated.py")));

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn walk_survives_symlink_loops() {
        let dir = scratch("symloop");
        let sub = dir.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("a.py"), "x = 1\n").unwrap();
        std::os::unix::fs::symlink(&dir, sub.join("loop")).unwrap();

        let map = default_extension_map();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        assert_eq!(collection.files.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn explicit_symlink_keeps_its_lexical_marker_path() {
        let dir = scratch("symlink_marker");
        let target_dir = dir.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("target.py");
        let alias = dir.join("alias.py");
        fs::write(&target, "print('target')\n").unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&alias), &map, None).unwrap();
        let combined =
            link_files(&collection.files, &collection.marker_root, &map, &backends).unwrap();
        assert!(combined.contains("# ── alias.py ──"));
        assert!(!combined.contains("# ── target/target.py ──"));

        let _ = fs::remove_dir_all(&dir);
    }

    // ── environment isolation and placement tests ───────────────────────────

    #[test]
    fn link_files_synthesizes_fresh_intent_without_numeric_identities() {
        let dir = scratch("env_ids");
        fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        fs::write(dir.join("b.py"), "y = 2\n").unwrap();
        fs::write(dir.join("c.sh"), "echo hi\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined =
            link_files(&collection.files, &collection.marker_root, &map, &backends).unwrap();

        assert_eq!(combined.matches("python[*]^(").count(), 2, "{combined}");
        assert_eq!(combined.matches(")_python[*]").count(), 2, "{combined}");
        assert!(combined.contains("bash[*]^("), "{combined}");
        assert!(!combined.contains("python[0]^(") && !combined.contains("python[1]^("));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn linker_isolated_environment_markers_parse_cleanly() {
        let dir = scratch("env_parse");
        fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        fs::write(dir.join("b.py"), "y = 2\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined =
            link_files(&collection.files, &collection.marker_root, &map, &backends).unwrap();

        // The combined output must parse without errors.
        let mut parser = o_lang::parser::Parser::new(&combined, &backends);
        parser
            .parse()
            .expect("combined output with linker-isolated environments should parse");

        let _ = fs::remove_dir_all(&dir);
    }

    // ── dependency ordering tests ────────────────────────────────────────────

    #[test]
    fn python_files_ordered_by_import_dependency() {
        let dir = scratch("pydeps");
        // b.py imports from a, so a.py should come first.
        fs::write(dir.join("a.py"), "def helper(): pass\n").unwrap();
        fs::write(dir.join("b.py"), "from a import helper\nhelper()\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined =
            link_files(&collection.files, &collection.marker_root, &map, &backends).unwrap();

        // a.py (the dependency) must appear before b.py in the output even
        // though both use the same non-identifying fresh marker.
        let pos_a = combined
            .find("# ── a.py ──")
            .expect("a.py marker not found");
        let pos_b = combined
            .find("# ── b.py ──")
            .expect("b.py marker not found");
        assert!(
            pos_a < pos_b,
            "a.py dependency should precede b.py; positions are a={} b={}",
            pos_a,
            pos_b
        );
        let slot0_start = combined[pos_a..].find("python[*]^(").unwrap() + pos_a;
        let slot0_end = combined[slot0_start..].find(")_python[*]").unwrap() + slot0_start;
        let slot0_body = &combined[slot0_start..slot0_end];
        assert!(
            slot0_body.contains("def helper"),
            "first fresh Python section should contain a.py"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn autonomous_parallel_wraps_fresh_sections_and_preserves_member_order() {
        let dir = scratch("parallel_autonomous");
        fs::write(dir.join("a.py"), "print('a')\n").unwrap();
        fs::write(dir.join("b.sh"), "printf b\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined = link_files_with_options(
            &collection.files,
            &collection.marker_root,
            &map,
            &backends,
            Some(ParallelLinkMode::Autonomous),
            true,
            false,
        )
        .unwrap();

        assert!(combined.contains("autonomous(batch("), "{combined}");
        assert_eq!(
            combined.matches("autonomous(batch(").count(),
            1,
            "independent sections should share one wave:\n{combined}"
        );
        assert!(combined.find("# ── a.py ──").unwrap() < combined.find("# ── b.sh ──").unwrap());
        Parser::new(&combined, &backends)
            .parse()
            .expect("parallel linked output should parse");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn autonomous_parallel_emits_dependency_chain_as_barrier_waves() {
        let dir = scratch("parallel_dependency_chain");
        fs::write(dir.join("a.py"), "VALUE = 40\n").unwrap();
        fs::write(dir.join("b.py"), "from a import VALUE\nNEXT = VALUE + 1\n").unwrap();
        fs::write(dir.join("c.py"), "from b import NEXT\nprint(NEXT + 1)\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined = link_files_with_options(
            &collection.files,
            &collection.marker_root,
            &map,
            &backends,
            Some(ParallelLinkMode::Autonomous),
            true,
            false,
        )
        .unwrap();

        let waves = combined
            .split("autonomous(batch(")
            .skip(1)
            .collect::<Vec<_>>();
        assert_eq!(
            waves.len(),
            3,
            "dependency chain must have three waves:\n{combined}"
        );
        assert!(waves[0].contains("# ── a.py ──"), "{combined}");
        assert!(waves[1].contains("# ── b.py ──"), "{combined}");
        assert!(waves[2].contains("# ── c.py ──"), "{combined}");
        Parser::new(&combined, &backends)
            .parse()
            .expect("dependency-wave output should parse");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn autonomous_parallel_keeps_independent_dependents_in_one_wave() {
        let dir = scratch("parallel_dependency_fanout");
        fs::write(dir.join("a.py"), "VALUE = 40\n").unwrap();
        fs::write(dir.join("b.py"), "from a import VALUE\nprint(VALUE + 1)\n").unwrap();
        fs::write(dir.join("c.py"), "from a import VALUE\nprint(VALUE + 2)\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined = link_files_with_options(
            &collection.files,
            &collection.marker_root,
            &map,
            &backends,
            Some(ParallelLinkMode::Autonomous),
            true,
            false,
        )
        .unwrap();

        let waves = combined
            .split("autonomous(batch(")
            .skip(1)
            .collect::<Vec<_>>();
        assert_eq!(waves.len(), 2, "fanout should use two waves:\n{combined}");
        assert!(waves[0].contains("# ── a.py ──"), "{combined}");
        assert!(waves[1].contains("# ── b.py ──"), "{combined}");
        assert!(waves[1].contains("# ── c.py ──"), "{combined}");
        Parser::new(&combined, &backends)
            .parse()
            .expect("fanout-wave output should parse");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn autonomous_parallel_serializes_dependency_cycles_in_input_order() {
        let dir = scratch("parallel_dependency_cycle");
        fs::write(dir.join("a.py"), "import b\nprint('a')\n").unwrap();
        fs::write(dir.join("b.py"), "import a\nprint('b')\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined = link_files_with_options(
            &collection.files,
            &collection.marker_root,
            &map,
            &backends,
            Some(ParallelLinkMode::Autonomous),
            true,
            false,
        )
        .unwrap();

        let waves = combined
            .split("autonomous(batch(")
            .skip(1)
            .collect::<Vec<_>>();
        assert_eq!(
            waves.len(),
            2,
            "cycle fallback must remain serial:\n{combined}"
        );
        assert!(waves[0].contains("# ── a.py ──"), "{combined}");
        assert!(waves[1].contains("# ── b.py ──"), "{combined}");
        Parser::new(&combined, &backends)
            .parse()
            .expect("cycle fallback output should parse");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn autonomous_parallel_keeps_cycle_dependents_after_the_cycle() {
        let dir = scratch("parallel_dependency_cycle_consumer");
        // Lexical order puts the consumer first. A plain Kahn-leftover
        // fallback would therefore emit it before the unresolved cycle.
        fs::write(
            dir.join("a_consumer.py"),
            // Depending on the first emitted member still means depending on
            // the whole SCC through the other member's back-edge.
            "from y_cycle import RIGHT\nprint(RIGHT)\n",
        )
        .unwrap();
        fs::write(
            dir.join("y_cycle.py"),
            "from z_cycle import VALUE\nRIGHT = VALUE\n",
        )
        .unwrap();
        fs::write(
            dir.join("z_cycle.py"),
            "from y_cycle import RIGHT\nVALUE = RIGHT\n",
        )
        .unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined = link_files_with_options(
            &collection.files,
            &collection.marker_root,
            &map,
            &backends,
            Some(ParallelLinkMode::Autonomous),
            true,
            false,
        )
        .unwrap();

        let pos_consumer = combined.find("# ── a_consumer.py ──").unwrap();
        let pos_y = combined.find("# ── y_cycle.py ──").unwrap();
        let pos_z = combined.find("# ── z_cycle.py ──").unwrap();
        assert!(pos_y < pos_z, "cycle fallback must preserve input order");
        assert!(
            pos_z < pos_consumer,
            "a dependent outside the SCC must follow the entire cycle:\n{combined}"
        );
        assert_eq!(
            combined.matches("autonomous(batch(").count(),
            3,
            "cycle members and their consumer each require a barrier wave:\n{combined}"
        );
        Parser::new(&combined, &backends)
            .parse()
            .expect("cycle-consumer output should parse");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn autonomous_parallel_emits_diamond_as_antichain_waves() {
        let dir = scratch("parallel_dependency_diamond");
        fs::write(dir.join("a.py"), "VALUE = 40\n").unwrap();
        fs::write(dir.join("b.py"), "from a import VALUE\nLEFT = VALUE + 1\n").unwrap();
        fs::write(dir.join("c.py"), "from a import VALUE\nRIGHT = VALUE + 2\n").unwrap();
        fs::write(
            dir.join("d.py"),
            "from b import LEFT\nfrom c import RIGHT\nprint(LEFT + RIGHT)\n",
        )
        .unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined = link_files_with_options(
            &collection.files,
            &collection.marker_root,
            &map,
            &backends,
            Some(ParallelLinkMode::Autonomous),
            true,
            false,
        )
        .unwrap();

        let waves = combined
            .split("autonomous(batch(")
            .skip(1)
            .collect::<Vec<_>>();
        assert_eq!(
            waves.len(),
            3,
            "diamond should use three waves:\n{combined}"
        );
        assert!(waves[0].contains("# ── a.py ──"), "{combined}");
        assert!(waves[1].contains("# ── b.py ──"), "{combined}");
        assert!(waves[1].contains("# ── c.py ──"), "{combined}");
        assert!(waves[2].contains("# ── d.py ──"), "{combined}");
        assert!(
            waves[1].find("# ── b.py ──").unwrap() < waves[1].find("# ── c.py ──").unwrap(),
            "antichain result members must retain deterministic input order"
        );
        Parser::new(&combined, &backends)
            .parse()
            .expect("diamond-wave output should parse");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verified_parallel_keeps_hosted_shims_sequential() {
        let dir = scratch("parallel_verified");
        fs::write(dir.join("a.html"), "<p>a</p>\n").unwrap();
        fs::write(dir.join("b.py"), "print('b')\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined = link_files_with_options(
            &collection.files,
            &collection.marker_root,
            &map,
            &backends,
            Some(ParallelLinkMode::Verified),
            false,
            false,
        )
        .unwrap();

        assert_eq!(
            combined.matches("autonomous(batch(").count(),
            1,
            "{combined}"
        );
        assert!(combined.contains("html[*]^("));
        assert!(combined.contains("python[*]^("));
        Parser::new(&combined, &backends)
            .parse()
            .expect("mixed verified output should parse");

        let required = link_files_with_options(
            &collection.files,
            &collection.marker_root,
            &map,
            &backends,
            Some(ParallelLinkMode::Verified),
            true,
            false,
        )
        .unwrap_err();
        assert!(required.to_string().contains("b.py"), "{required:#}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_import_ordering_import_statement() {
        let dir = scratch("pydeps_import");
        // c.py imports from b, b imports from a.
        fs::write(dir.join("c.py"), "import b\n").unwrap();
        fs::write(dir.join("b.py"), "import a\n").unwrap();
        fs::write(dir.join("a.py"), "VALUE = 42\n").unwrap();

        let map = default_extension_map();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let ordered = order_by_deps(&collection.files, &map);

        // The expected order after topo-sort: a.py, b.py, c.py.
        let names: Vec<&str> = ordered
            .iter()
            .filter_map(|p| p.file_name()?.to_str())
            .collect();
        assert_eq!(
            names,
            ["a.py", "b.py", "c.py"],
            "expected topo order a<b<c, got {:?}",
            names
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dependency_ordering_covers_hosted_language_import_forms() {
        let cases = [
            ("a.py", "z.py", "from .z import value\n"),
            ("a.js", "z.js", "import { value } from './z.js';\n"),
            ("a.rs", "z.rs", "mod z;\n"),
            ("a.c", "z.h", "#include \"z.h\"\n"),
            ("A.java", "Z.java", "import local.Z;\n"),
            ("A.hs", "Z.hs", "import Z\n"),
            ("a.rb", "z.rb", "require_relative './z'\n"),
            ("a.ml", "z.ml", "open Z\n"),
            ("a.rkt", "z.rkt", "(require \"z.rkt\")\n"),
            ("a.sh", "z.sh", "source ./z.sh\n"),
            ("a.nix", "z.nix", "let z = import ./z.nix; in z\n"),
            ("A.cs", "Z.cs", "using Z;\n"),
            ("a.m", "z.m", "run('z.m')\n"),
            ("a.wl", "z.wl", "Get[\"z.wl\"]\n"),
        ];

        for (importer, dependency, source) in cases {
            let dir = scratch(&format!("deps_{}", importer.replace('.', "_")));
            fs::write(dir.join(importer), source).unwrap();
            fs::write(dir.join(dependency), "dependency\n").unwrap();

            let map = default_extension_map();
            let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
            let ordered = order_by_deps(&collection.files, &map);
            let names = ordered
                .iter()
                .filter_map(|path| path.file_name()?.to_str())
                .collect::<Vec<_>>();
            let dependency_position = names.iter().position(|name| *name == dependency).unwrap();
            let importer_position = names.iter().position(|name| *name == importer).unwrap();
            assert!(
                dependency_position < importer_position,
                "{importer} did not follow {dependency}: {names:?}"
            );

            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn dependency_ordering_does_not_reorder_different_languages() {
        let dir = scratch("crosslang");
        fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        fs::write(dir.join("b.sh"), "echo hi\n").unwrap();
        fs::write(dir.join("c.py"), "import a\n").unwrap();

        let map = default_extension_map();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let ordered = order_by_deps(&collection.files, &map);

        // a.py and c.py are Python; b.sh is bash.
        // After ordering: a.py (py dep) before c.py; b.sh keeps its position.
        let names: Vec<&str> = ordered
            .iter()
            .filter_map(|p| p.file_name()?.to_str())
            .collect();
        let pos_a = names.iter().position(|&n| n == "a.py").unwrap();
        let pos_c = names.iter().position(|&n| n == "c.py").unwrap();
        assert!(pos_a < pos_c, "a.py must come before c.py, got {:?}", names);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cyclic_deps_do_not_panic() {
        let dir = scratch("cycle");
        fs::write(dir.join("a.py"), "import b\n").unwrap();
        fs::write(dir.join("b.py"), "import a\n").unwrap();

        let map = default_extension_map();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        // Should not panic; result has both files.
        let ordered = order_by_deps(&collection.files, &map);
        assert_eq!(ordered.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }
}
