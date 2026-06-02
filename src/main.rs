use anyhow::{Context, Result};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;

use o_lang::eval::Evaluator;
use o_lang::parser::Parser;
use o_lang::value::OValue;

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
        "nix_expr",
        "nix_store",
        "nixos_test",
        "text",
        // Major languages — full subprocess execution via backend shims.
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
        // quote^ captures its body as an unevaluated OValue::Expr without
        // calling any subprocess shim. O.eval(q) in a Python block sends the
        // source back to the runtime for evaluation via the eval_request
        // wire protocol.
        "quote",
        // NOTE: `lazy` is NOT a language. It's a builtin CALL — `lazy(expr)` —
        // because there is no source text in any language called "lazy"; the
        // body is already O-level statements with a different evaluation
        // policy. Implementing it as a block-shaped construct would be a
        // category error: blocks are languages, lazy isn't one.
    ]
    .into_iter()
    .map(String::from)
    .collect();

    let mut parser = Parser::new(&source, &registered_backends);
    let nodes = parser.parse().context("failed to parse .O source")?;

    let mut evaluator = Evaluator::new(shim_dir)
        .with_registered_backends(registered_backends);
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
