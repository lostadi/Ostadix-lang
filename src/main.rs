use anyhow::{Context, Result};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;

mod value;
mod parser;
mod process;
mod eval;

use eval::Evaluator;
use parser::Parser;
use value::OValue;

fn main() -> Result<()> {
    let mut args = env::args().skip(1);

    let input_path = args.next().context(
        "usage: cargo run -- <file.O> [shim_dir]\nexample: cargo run -- examples/hello.O backends"
    )?;

    let shim_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("backends"));

    let mut source = fs::read_to_string(&input_path)
        .with_context(|| format!("failed to read input file: {}", input_path))?;

    if source.starts_with("#!") {
        if let Some(newline) = source.find('\n') {
            source = source[newline + 1..].to_string();
        } else {
            source.clear();
        }
    }

    let registered_backends: HashSet<String> = [
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
        "nix_store",
        "nixos_test",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    let mut parser = Parser::new(&source, &registered_backends);
    let nodes = parser.parse().context("failed to parse .O source")?;

    let mut evaluator = Evaluator::new(shim_dir);
    let result = evaluator
        .eval_document(nodes)
        .context("failed to evaluate .O document")?;

    match result {
        OValue::Str { v } | OValue::Html { v } => {
            print!("{v}");
        }
        other => {
            println!("{:#?}", other);
        }
    }

    Ok(())
}
