// ─────────────────────────────────────────────────────────────────────────────
// olangc — the O-lang compiler
//
// Compiles a .O source file into a self-contained native binary.
//
// Usage:
//   olangc <input.O>                       # output binary named after input stem
//   olangc <input.O> -o myprogram          # explicit output name
//   olangc <input.O> --shim-dir ./backends # custom shim directory
//
// What it does:
//   1. Reads the .O source file.
//   2. Resolves backend shim scripts: starts from shims that are bundled into
//      olangc itself at olangc's compile time (so olangc works from any cwd
//      with no adjacent backends/ directory), then optionally overlays files
//      from --shim-dir if the user passed one.
//   3. Creates a temporary Cargo project that bundles:
//        - All O-lang runtime source files (embedded in olangc at its own
//          compile time via include_str!, so olangc is self-contained).
//        - The .O source file (copied as "program.O" in the generated src/).
//        - All shim scripts (copied into src/shims/).
//        - A generated main.rs that references them via include_str!/include_bytes!.
//        - A Cargo.toml mirroring the runtime's dependencies.
//        - The workspace Cargo.lock so dependency resolution is instant and
//          reproducible (embedded in olangc at its own compile time).
//   4. Runs `cargo build --release` in the temp project.
//   5. Copies the resulting binary to the requested output path.
//
// The output binary is fully self-contained at the Rust level: it has no
// dependency on the .O source file, the backends/ directory, or the olangc
// tool itself.  At runtime it still needs the language runtimes that the .O
// program uses — Python for python^ blocks, Nix for nix^ blocks, etc. — for
// the same reason that a compiled C program that calls system("python3 ...")
// still needs Python.  Those runtimes are extracted from the binary into a
// per-invocation temp directory and cleaned up on exit.
// ─────────────────────────────────────────────────────────────────────────────

use anyhow::{bail, Context, Result};
use clap::Parser as ClapParser;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// ─────────────────────────────────────────────────────────────────────────────
// Runtime source files — embedded at olangc's own compile time.
//
// These are written verbatim into the temp project so the generated binary
// gets an identical copy of the O-lang runtime.  When the runtime changes,
// olangc must be recompiled for those changes to appear in newly compiled
// .O programs.
// ─────────────────────────────────────────────────────────────────────────────

const RUNTIME_VALUE_RS:     &str = include_str!("../value.rs");
const RUNTIME_PARSER_RS:    &str = include_str!("../parser.rs");
const RUNTIME_EVAL_RS:      &str = include_str!("../eval.rs");
const RUNTIME_PROCESS_RS:   &str = include_str!("../process.rs");
const RUNTIME_NIX_OPS_RS:   &str = include_str!("../nix_ops.rs");
const RUNTIME_NIXOS_OPS_RS: &str = include_str!("../nixos_ops.rs");

// Cargo.lock from the workspace — embedded so the temp project gets identical
// dependency versions on first build without a network round-trip.
const WORKSPACE_CARGO_LOCK: &[u8] = include_bytes!("../../Cargo.lock");

// ─────────────────────────────────────────────────────────────────────────────
// Bundled backend shim scripts — embedded at olangc's own compile time.
//
// olangc is the single source of truth for shim contents: every compiled .O
// binary gets these scripts extracted to its per-invocation temp directory
// at runtime.  Bundling the shims into olangc itself (rather than re-reading
// them from a `backends/` directory next to the cwd at every invocation) is
// what makes `olangc foo.O` work from any directory, including one with no
// adjacent `backends/`.
//
// To replace a shim during development, pass `--shim-dir <path>`; any file
// in that directory whose name matches a bundled shim overrides it, and any
// file with a new name is appended to the embedded set.
const BUNDLED_SHIMS: &[(&str, &[u8])] = &[
    ("nix_shim.py",        include_bytes!("../../backends/nix_shim.py")),
    ("nix_store_shim.py",  include_bytes!("../../backends/nix_store_shim.py")),
    ("nixos_test_shim.py", include_bytes!("../../backends/nixos_test_shim.py")),
    ("python_shim.py",     include_bytes!("../../backends/python_shim.py")),
];

// ─────────────────────────────────────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────────────────────────────────────

#[derive(ClapParser, Debug)]
#[command(
    name    = "olangc",
    about   = "Compile a .O program into a self-contained native binary",
    long_about = "\
Compiles a .O source file into a native binary.  The binary embeds the program \
source, all backend shim scripts, and the O-lang runtime.  It still requires \
the language runtimes used by the program (e.g. Python, Nix) to be installed \
on the target machine.",
)]
struct Cli {
    /// The .O source file to compile
    input: PathBuf,

    /// Output binary path (default: input file stem in the current directory)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Override or extend the bundled backend shim scripts with files from
    /// this directory.  Files with names matching a bundled shim replace it;
    /// files with new names are added.  If omitted, olangc uses only its
    /// built-in shims, so it works from any working directory.
    #[arg(long)]
    shim_dir: Option<PathBuf>,

    /// Keep the intermediate build directory after compilation (useful for debugging)
    #[arg(long)]
    keep_build_dir: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Resolve output path: default to <input stem> in cwd.
    let output = match cli.output {
        Some(p) => p,
        None => {
            let stem = cli.input
                .file_stem()
                .with_context(|| format!("input path has no file stem: {}", cli.input.display()))?
                .to_string_lossy();
            PathBuf::from(stem.as_ref())
        }
    };

    let source = fs::read_to_string(&cli.input)
        .with_context(|| format!("failed to read {}", cli.input.display()))?;

    let shims = read_shims(cli.shim_dir.as_deref())?;

    let build_dir = create_build_dir()?;
    eprintln!("olangc: building in {}", build_dir.display());
    eprintln!("olangc: embedding {} shim script(s)", shims.len());

    let result = assemble_and_compile(&cli.input, &source, &shims, &build_dir, &output);

    if !cli.keep_build_dir {
        let _ = fs::remove_dir_all(&build_dir);
    } else {
        eprintln!("olangc: keeping build directory: {}", build_dir.display());
    }

    result
}

// ─────────────────────────────────────────────────────────────────────────────
// Core compilation logic
// ─────────────────────────────────────────────────────────────────────────────

fn assemble_and_compile(
    input_path: &Path,
    source:     &str,
    shims:      &[(String, Vec<u8>)],
    build_dir:  &Path,
    output:     &Path,
) -> Result<()> {
    let bin_name = derive_bin_name(output);
    let src_dir  = build_dir.join("src");
    let shim_dir = src_dir.join("shims");
    fs::create_dir_all(&shim_dir)?;

    // ── Runtime source files ─────────────────────────────────────────────────
    fs::write(src_dir.join("value.rs"),     RUNTIME_VALUE_RS)?;
    fs::write(src_dir.join("parser.rs"),    RUNTIME_PARSER_RS)?;
    fs::write(src_dir.join("eval.rs"),      RUNTIME_EVAL_RS)?;
    fs::write(src_dir.join("process.rs"),   RUNTIME_PROCESS_RS)?;
    fs::write(src_dir.join("nix_ops.rs"),   RUNTIME_NIX_OPS_RS)?;
    fs::write(src_dir.join("nixos_ops.rs"), RUNTIME_NIXOS_OPS_RS)?;

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

    // ── Generated main.rs ────────────────────────────────────────────────────
    let main_rs = generate_main_rs(&program_filename, &shim_include_lines);
    fs::write(src_dir.join("main.rs"), &main_rs)?;

    // ── Cargo.toml ───────────────────────────────────────────────────────────
    fs::write(build_dir.join("Cargo.toml"), generate_cargo_toml(&bin_name))?;

    // ── Cargo.lock — embed workspace lock for reproducible/fast first build ──
    fs::write(build_dir.join("Cargo.lock"), WORKSPACE_CARGO_LOCK)?;

    // ── Build ────────────────────────────────────────────────────────────────
    eprintln!("olangc: running cargo build --release ...");
    let status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(build_dir)
        .status()
        .context("failed to spawn cargo — is Rust/Cargo installed?")?;

    if !status.success() {
        bail!("cargo build --release failed (see output above)");
    }

    // ── Copy binary to output ────────────────────────────────────────────────
    let built = built_binary_path(build_dir, &bin_name);
    let dest  = canonicalize_output(output)?;

    fs::copy(&built, &dest)
        .with_context(|| format!("failed to copy {} → {}", built.display(), dest.display()))?;

    // Make the output binary executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
    }

    eprintln!("olangc: compiled → {}", dest.display());
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Code generation
// ─────────────────────────────────────────────────────────────────────────────

fn generate_main_rs(program_filename: &str, shim_include_lines: &[String]) -> String {
    let shim_entries = if shim_include_lines.is_empty() {
        "    // no shims bundled".to_string()
    } else {
        shim_include_lines.join("\n")
    };

    // NOTE: `{{` / `}}` are literal `{` / `}` in a format! string.
    // We use r###"..."### (three hashes) so that `"#` sequences inside the
    // generated code (e.g., `starts_with("#!")`) don't prematurely end the
    // raw-string delimiter.
    format!(
        r###"// AUTO-GENERATED by olangc. DO NOT EDIT.
mod value;
mod parser;
mod eval;
mod process;
mod nix_ops;
mod nixos_ops;

use eval::Evaluator;
use parser::Parser;
use value::OValue;
use std::collections::HashSet;

/// The compiled .O program source, embedded at compile time.
const PROGRAM_SOURCE: &str = include_str!({program_filename:?});

/// Backend shim scripts, embedded as raw bytes at compile time.
/// Extracted to a per-invocation temp directory at startup and cleaned up on exit.
const EMBEDDED_SHIMS: &[(&str, &[u8])] = &[
{shim_entries}
];

struct ShimGuard(std::path::PathBuf);

impl Drop for ShimGuard {{
    fn drop(&mut self) {{
        let _ = std::fs::remove_dir_all(&self.0);
    }}
}}

fn main() -> anyhow::Result<()> {{
    use anyhow::Context as _;

    // Extract embedded shims to a private temp directory for this invocation.
    let shim_dir = std::env::temp_dir()
        .join(format!("o_shims_{{}}", std::process::id()));
    std::fs::create_dir_all(&shim_dir)?;
    let _guard = ShimGuard(shim_dir.clone());

    for (name, content) in EMBEDDED_SHIMS {{
        let dest = shim_dir.join(name);
        std::fs::write(&dest, content)
            .with_context(|| format!("failed to extract shim {{name}}"))?;
        #[cfg(unix)]
        {{
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
        }}
    }}

    let registered_backends: HashSet<String> = [
        "O", "python", "html", "latex", "markdown", "bash", "shell",
        "rust", "racket", "nix", "nix_expr", "nix_store", "nixos_test",
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

    let mut evaluator = Evaluator::new(shim_dir);
    let result = evaluator
        .eval_document(nodes)
        .context("failed to evaluate program")?;

    match result {{
        OValue::Str {{ v }} | OValue::Html {{ v }} => print!("{{v}}"),
        other => println!("{{:#?}}", other),
    }}

    Ok(())
}}
"###,
        program_filename = program_filename,
        shim_entries     = shim_entries,
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
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve the shim set used for a compilation.
///
/// Always starts from `BUNDLED_SHIMS` (embedded into olangc at its own
/// compile time).  If `override_dir` is `Some(path)`, every file in that
/// directory is read and overlaid on top: a file with the same name as a
/// bundled shim replaces it; a file with a new name is appended.  Result is
/// sorted by name (via the BTreeMap).
///
/// A missing `override_dir` is an error rather than a silent empty fallback,
/// because silent fallback is exactly what produced the prior bug where
/// `olangc foo.O` from a directory without an adjacent `backends/` emitted
/// a binary with zero shims that died at runtime with a cryptic
/// `python_shim.py: No such file or directory`.
fn read_shims(override_dir: Option<&Path>) -> Result<Vec<(String, Vec<u8>)>> {
    use std::collections::BTreeMap;

    let mut by_name: BTreeMap<String, Vec<u8>> = BUNDLED_SHIMS
        .iter()
        .map(|(name, bytes)| ((*name).to_string(), bytes.to_vec()))
        .collect();

    if let Some(dir) = override_dir {
        if !dir.exists() {
            bail!(
                "shim directory '{}' does not exist (omit --shim-dir to use bundled shims)",
                dir.display()
            );
        }
        for entry in fs::read_dir(dir)
            .with_context(|| format!("failed to read shim directory: {}", dir.display()))?
        {
            let entry = entry?;
            let path  = entry.path();
            if path.is_file() {
                let name    = path.file_name().unwrap().to_string_lossy().into_owned();
                let content = fs::read(&path)
                    .with_context(|| format!("failed to read shim: {}", path.display()))?;
                by_name.insert(name, content);
            }
        }
    }

    Ok(by_name.into_iter().collect())
}

/// Create a fresh temporary build directory with a unique name.
fn create_build_dir() -> Result<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir()
        .join(format!("olang_build_{}_{}", std::process::id(), ts));
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
fn built_binary_path(build_dir: &Path, bin_name: &str) -> PathBuf {
    let name = if cfg!(windows) {
        format!("{bin_name}.exe")
    } else {
        bin_name.to_string()
    };
    build_dir.join("target").join("release").join(name)
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
