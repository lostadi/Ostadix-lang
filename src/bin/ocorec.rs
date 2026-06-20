use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use o_lang::ocore::{compile, CompileOptions, EmitKind, Target};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Emit {
    Ast,
    Hir,
    Mir,
    Asm,
    Obj,
}

#[derive(Debug, Parser)]
#[command(
    name = "ocorec",
    version,
    about = "Compile statically typed O-core to freestanding native objects"
)]
struct Cli {
    /// One or more .oc source modules in the same compilation unit.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Output kind.
    #[arg(long, value_enum, default_value_t = Emit::Obj)]
    emit: Emit,

    /// Compilation target. The initial implementation accepts only x86_64-unknown-none.
    #[arg(long, default_value = "x86_64-unknown-none")]
    target: String,

    /// Output path, or `-` for textual output on stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Retain generated assembly next to an object output.
    #[arg(long)]
    keep_asm: bool,
}

fn main() {
    let cli = Cli::parse();
    if cli.target != "x86_64-unknown-none" && cli.target != "x86_64-unknown-none-elf" {
        eprintln!(
            "ocorec: unsupported target `{}`; expected x86_64-unknown-none",
            cli.target
        );
        std::process::exit(2);
    }
    let emit = match cli.emit {
        Emit::Ast => EmitKind::Ast,
        Emit::Hir => EmitKind::Hir,
        Emit::Mir => EmitKind::Mir,
        Emit::Asm => EmitKind::Assembly,
        Emit::Obj => EmitKind::Object,
    };
    let output = cli
        .output
        .unwrap_or_else(|| default_output(&cli.inputs[0], emit));
    let options = CompileOptions {
        target: Target::X86_64UnknownNone,
        emit,
        output,
        keep_assembly: cli.keep_asm,
    };
    match compile(&cli.inputs, &options) {
        Ok(result) => {
            if result.output.as_path() != std::path::Path::new("-") {
                eprintln!("ocorec: wrote {}", result.output.display());
            }
            if let Some(assembly) = result.assembly.filter(|p| p != &result.output) {
                eprintln!("ocorec: kept {}", assembly.display());
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}

fn default_output(input: &std::path::Path, emit: EmitKind) -> PathBuf {
    let mut output = PathBuf::from(input.file_stem().and_then(|s| s.to_str()).unwrap_or("a"));
    output.set_extension(match emit {
        EmitKind::Ast => "ast",
        EmitKind::Hir => "hir",
        EmitKind::Mir => "mir",
        EmitKind::Assembly => "s",
        EmitKind::Object => "o",
    });
    output
}
