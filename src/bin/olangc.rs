// ─────────────────────────────────────────────────────────────────────────────
// olangc — the O-lang compiler
//
// Compiles a .O source file to a hosted native or WASI binary, executes its
// OIR directly in-process, or prints that same executable OIR and plan.
//
// Usage:
//   olangc <input.O>                              # binary target (default)
//   olangc <input.O> -o myprogram                 # explicit output name
//   olangc <input.O> --target wasm                # wasm32-wasip1
//   olangc <input.O> --target script              # run in-process
//   olangc <input.O> --target ir                  # dump the lowered OIR
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
//        - All O-lang runtime source files (embedded in olangc at its own
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

// ─────────────────────────────────────────────────────────────────────────────
// Runtime source files — embedded at olangc's own compile time.
//
// These are written verbatim into the temp project so the generated binary
// gets an identical copy of the O-lang runtime.  When the runtime changes,
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
    /// Lower the parsed program to OIR, build its ExecutionPlan, and print
    /// both to stdout without executing them.
    Ir,
}

#[derive(ClapParser, Debug)]
#[command(
    name = "olangc",
    about = "Compile or run a .O program",
    long_about = "\
Compiles a .O source file into a native binary (--target binary, the default), \
a wasm32-wasip1 module (--target wasm), or executes executable OIR in-process \
(--target script). Binary outputs embed the program source, compatibility adapters, and \
the O-lang runtime. In ir mode the same OIR and ExecutionPlan used at runtime \
are printed without execution."
)]
struct Cli {
    /// The .O source file to compile or run
    input: PathBuf,

    /// Compilation target
    #[arg(long, value_enum, default_value_t = CompileTarget::Binary)]
    target: CompileTarget,

    /// Output binary path (default: input file stem in the current directory).
    /// Ignored when --target is "script" or "ir".
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
    /// Format: NAME=LANG[:fs_read,fs_write,network,process].
    #[arg(long = "backend-grant")]
    backend_grants: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    if o_lang::backend::run_backend_from_env_args()? {
        return Ok(());
    }

    let cli = Cli::parse();

    let source = fs::read_to_string(&cli.input)
        .with_context(|| format!("failed to read {}", cli.input.display()))?;

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
        CompileTarget::Ir => dump_ir(&source),
    }
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
    let lib_rs = generate_lib_rs();
    fs::write(src_dir.join("lib.rs"), &lib_rs)?;
    let main_rs = generate_main_rs(
        &bin_name,
        &program_filename,
        &shim_include_lines,
        backend_grants,
    );
    fs::write(src_dir.join("main.rs"), &main_rs)?;

    // ── Cargo.toml ───────────────────────────────────────────────────────────
    fs::write(build_dir.join("Cargo.toml"), generate_cargo_toml(&bin_name))?;

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

// ─────────────────────────────────────────────────────────────────────────────
// Target B — execute in-process (script mode)
//
// The O-lang runtime (parser, evaluator, value system) is already compiled
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
// the ONode forest to OIR (see src/ir.rs), and prints the lowered program.
// Purely an inspection/debugging surface: nothing is executed and no output
// file is produced.
// ─────────────────────────────────────────────────────────────────────────────

fn dump_ir(source: &str) -> Result<()> {
    let src = strip_shebang(source);
    let registered_backends = registered_backends();

    let mut parser = Parser::new(&src, &registered_backends);
    let nodes = parser.parse().context("failed to parse .O source")?;

    let program = OIrProgram::lower(&nodes);
    print!("{}", program.to_text());
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared front-end helpers
// ─────────────────────────────────────────────────────────────────────────────

/// The backend names accepted in language tags — same set as the O interpreter.
fn registered_backends() -> HashSet<String> {
    [
        "O",
        "python",
        "html",
        "latex",
        "markdown",
        "bash",
        "shell",
        "rust",
        "racket",
        "nix",
        "nix_expr",
        "nix_store",
        "nixos_test",
        "text",
        "csharp",
        "cpp",
        "haskell",
        "lisp",
        "common_lisp",
        "sql",
        "ruby",
        "matlab",
        "mathematica",
        "webassembly",
        "java",
        "javascript",
        "ocaml",
        "quote",
        // Aliases (canonicalized by the parser via the BackendRegistry).
        "py",
        "md",
        "tex",
        "plain",
        "o",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
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

fn generate_lib_rs() -> String {
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
pub mod eval;
pub mod process;
pub mod nix_ops;
pub mod nixos_ops;
pub mod scheduler;
pub(crate) mod wire;
"
    .to_string()
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

    let registered_backends: HashSet<String> = [
        "O", "python", "html", "latex", "markdown", "bash", "shell",
        "rust", "racket", "nix", "nix_expr", "nix_store", "nixos_test",
        "text",
        "csharp", "cpp", "haskell", "lisp", "common_lisp", "sql",
        "ruby", "matlab", "mathematica", "webassembly", "java",
        "javascript", "ocaml",
        "quote",
        // Aliases (canonicalized by the parser via the BackendRegistry).
        "py", "md", "tex", "plain", "o",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

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

fn generate_cargo_toml(bin_name: &str) -> String {
    // Keep dependency versions in sync with the workspace Cargo.toml.
    // The Cargo.lock (embedded above) pins exact versions, so this just
    // needs to be a compatible range — which the workspace lock already satisfies.
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
num-bigint = {{ version = "0.4", features = ["serde"] }}
num-traits = "0.2"
getrandom  = "0.4.3"
thiserror  = "2"
anyhow     = "1"
clap       = {{ version = "4", features = ["derive"] }}

[profile.release]
opt-level     = 3
lto           = "fat"
codegen-units = 1
panic         = "abort"
strip         = "symbols"
"#,
        bin_name = bin_name,
        lib_name = bin_name.replace('-', "_"),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Create a fresh temporary build directory with a unique name.
fn create_build_dir() -> Result<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("olang_build_{}_{}", std::process::id(), ts));
    fs::create_dir_all(&dir)?;
    Ok(dir)
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
