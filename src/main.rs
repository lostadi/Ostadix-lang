use anyhow::{bail, Context, Result};
use rustyline::{error::ReadlineError, DefaultEditor};
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Instant;

use o_lang::eval::Evaluator;
use o_lang::parser::Parser;
use o_lang::shims::ExtractedShims;
use o_lang::value::OValue;

fn main() -> Result<()> {
    if o_lang::backend::run_backend_from_env_args()? {
        return Ok(());
    }

    let mut args = env::args().skip(1).collect::<VecDeque<_>>();
    let backends = registered_backends();
    let mut backend_grants = Vec::new();
    let mut json_output = false;
    let mut check_only = false;
    let mut eval_source: Option<String> = None;
    let mut local_workers = None;
    while args.front().is_some_and(|arg| {
        matches!(
            arg.as_str(),
            "--backend-grant" | "--executor" | "--workers" | "--json" | "--check" | "--eval" | "-e"
        )
    }) {
        match args.pop_front().unwrap().as_str() {
            "--backend-grant" => backend_grants.push(
                args.pop_front()
                    .context("--backend-grant requires NAME=LANG[:RIGHT,...]")?,
            ),
            "--json" => json_output = true,
            "--check" => check_only = true,
            "--eval" | "-e" => {
                eval_source = Some(
                    args.pop_front()
                        .context("--eval requires an O expression")?,
                );
            }
            "--executor" => {
                let choice = args
                    .pop_front()
                    .context("--executor requires `serial` or `graph`")?;
                match choice.as_str() {
                    "serial" => env::set_var("O_EXECUTOR", "serial"),
                    "graph" => env::set_var("O_EXECUTOR", "graph"),
                    other => bail!("unknown --executor value `{other}` (expected serial or graph)"),
                }
            }
            "--workers" => {
                let raw = args
                    .pop_front()
                    .context("--workers requires a positive integer")?;
                let workers = raw
                    .parse::<usize>()
                    .with_context(|| format!("invalid --workers value `{raw}`"))?;
                if workers == 0 {
                    bail!("--workers must be at least 1");
                }
                local_workers = Some(workers);
            }
            _ => unreachable!(),
        }
    }

    // No args in an interactive terminal → REPL.
    // In non-interactive contexts, missing args is a usage error so shell tests
    // and scripts do not silently enter and exit the REPL.
    match args.front().map(String::as_str) {
        Some("--help") | Some("-h") => {
            print_usage(&mut io::stdout())?;
            return Ok(());
        }
        None if eval_source.is_none()
            && io::stdin().is_terminal()
            && io::stderr().is_terminal() =>
        {
            let (shim_dir, _shim_guard) = resolve_shim_dir(None)?;
            return run_repl(shim_dir, backends, &backend_grants);
        }
        None if eval_source.is_none() => {
            print_usage(&mut io::stderr())?;
            bail!("missing input file (pass a .O file or use --repl)");
        }
        Some("--repl") | Some("-i") => {
            args.pop_front();
            let (shim_dir, _shim_guard) = resolve_shim_dir(args.pop_front().map(PathBuf::from))?;
            if let Some(extra) = args.pop_front() {
                print_usage(&mut io::stderr())?;
                bail!("unexpected extra argument after --repl: {}", extra);
            }
            return run_repl(shim_dir, backends, &backend_grants);
        }
        _ => {}
    }

    let (input_path, mut source) = match eval_source {
        Some(src) => ("<eval>".to_string(), src),
        None => {
            let path = args.pop_front().unwrap();
            let text = fs::read_to_string(&path)
                .with_context(|| format!("failed to read input file: {}", path))?;
            (path, text)
        }
    };
    let (shim_dir, _shim_guard) = resolve_shim_dir(args.pop_front().map(PathBuf::from))?;
    if let Some(extra) = args.pop_front() {
        print_usage(&mut io::stderr())?;
        bail!("unexpected extra argument: {}", extra);
    }

    if source.starts_with("#!") {
        source = source
            .find('\n')
            .map(|nl| source[nl + 1..].to_string())
            .unwrap_or_default();
    }

    let start = Instant::now();
    let mut parser = Parser::new(&source, &backends);
    let nodes = match parser.parse() {
        Ok(nodes) => nodes,
        Err(e) => return fail_stage(json_output, "parse", e),
    };

    if check_only {
        if json_output {
            println!(
                "{}",
                serde_json::json!({ "ok": true, "stage": "parse", "input": input_path })
            );
        } else {
            println!("ok");
        }
        return Ok(());
    }

    let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends);
    if let Some(workers) = local_workers {
        evaluator = evaluator.with_local_worker_parallelism(workers);
    }
    let mut scope = HashMap::new();
    for grant in &backend_grants {
        evaluator.install_backend_grant(grant, &mut scope)?;
    }
    let result = match evaluator.eval_document_with_scope(nodes, &mut scope) {
        Ok(result) => result,
        Err(e) => return fail_stage(json_output, "eval", e),
    };

    let elapsed = start.elapsed();
    if json_output {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "value": result,
                "type": result.type_name(),
                "elapsed_ms": elapsed.as_millis() as u64,
            })
        );
    } else {
        print_result(&result);
    }

    if !json_output && io::stderr().is_terminal() {
        if elapsed.as_millis() < 1000 {
            eprintln!("\x1b[2m  {} ms\x1b[0m", elapsed.as_millis());
        } else {
            eprintln!("\x1b[2m  {:.2} s\x1b[0m", elapsed.as_secs_f64());
        }
    }

    Ok(())
}

/// Report a parse or eval failure. In `--json` mode a structured error object
/// is printed to stdout so agents and tooling can consume it; the process
/// still exits non-zero in both modes.
fn fail_stage(json_output: bool, stage: &str, err: anyhow::Error) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::json!({ "ok": false, "stage": stage, "error": format!("{err:#}") })
        );
    }
    Err(err.context(match stage {
        "parse" => "failed to parse .O source",
        "eval" => "failed to evaluate .O document",
        _ => "failed to run .O source",
    }))
}

fn print_usage(out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "Usage:")?;
    writeln!(out, "  O <input.O> [backends_dir]")?;
    writeln!(out, "  O --repl [backends_dir]")?;
    writeln!(
        out,
        "  O --eval '<expr>' | -e '<expr>'      # evaluate an inline O expression"
    )?;
    writeln!(
        out,
        "  O --json <input.O>                   # machine-readable JSON result/error on stdout"
    )?;
    writeln!(
        out,
        "  O --check <input.O>                  # parse-only validation (combine with --json)"
    )?;
    writeln!(
        out,
        "  O --backend-grant NAME=LANG[:RIGHT,...] <input.O> [backends_dir]  # compatibility"
    )?;
    writeln!(
        out,
        "  O --executor serial|graph <input.O> [backends_dir]  # select execution engine (default: graph)"
    )?;
    writeln!(
        out,
        "  O --workers N <input.O> [backends_dir]        # bound graph local-worker concurrency"
    )?;
    writeln!(out, "  O --help")?;
    writeln!(out)?;
    writeln!(out, "Runs a .O file or starts the interactive REPL.")?;
    writeln!(
        out,
        "With no arguments in an interactive terminal, O starts the REPL. Backend grants are optional compatibility hooks; shim backends have default host authority."
    )?;
    Ok(())
}

fn resolve_shim_dir(explicit: Option<PathBuf>) -> Result<(PathBuf, Option<ExtractedShims>)> {
    if let Some(path) = explicit {
        return Ok((path, None));
    }

    if let Ok(path) = env::var("O_BACKENDS_DIR").or_else(|_| env::var("BACKENDS_DIR")) {
        return Ok((PathBuf::from(path), None));
    }

    let extracted = o_lang::shims::extract_bundled_shims("o_shims")
        .context("failed to extract bundled backend shims")?;
    Ok((extracted.path().to_path_buf(), Some(extracted)))
}

// ─── REPL ─────────────────────────────────────────────────────────────────────

fn run_repl(shim_dir: PathBuf, backends: HashSet<String>, backend_grants: &[String]) -> Result<()> {
    let color = io::stderr().is_terminal();
    let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
    let mut scope: HashMap<String, OValue> = HashMap::new();
    for grant in backend_grants {
        evaluator.install_backend_grant(grant, &mut scope)?;
    }
    let host_scope = scope.clone();

    if color {
        eprintln!(
            "\x1b[1m\x1b[34m  O ◦ lang\x1b[0m \x1b[2mREPL\x1b[0m  \
             \x1b[90m:q quit  :r reset  :scope vars  :? help\x1b[0m"
        );
    } else {
        eprintln!("O · lang REPL  :q quit  :r reset  :scope vars  :? help");
    }
    eprintln!();

    // Set up rustyline editor with history
    let mut rl = DefaultEditor::new()?;
    let history_path = std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".o_history"));
    if let Some(ref p) = history_path {
        let _ = rl.load_history(p);
    }

    let mut buf = String::new(); // accumulated multi-line input
    let mut cont = false; // in a continuation (unclosed expression)

    loop {
        let prompt = if cont { "  ... " } else { "O> " };

        match rl.readline(prompt) {
            Err(ReadlineError::Interrupted) => {
                // Ctrl+C — cancel current input, return to fresh prompt
                buf.clear();
                cont = false;
                continue;
            }
            Err(ReadlineError::Eof) => break, // Ctrl+D
            Err(e) => return Err(e.into()),
            Ok(line) => {
                let trimmed = line.trim();

                // Top-level commands — only at a fresh prompt (not mid-continuation)
                if !cont {
                    match trimmed {
                        ":q" | ":quit" | "exit" | "quit" => break,

                        ":r" | ":reset" => {
                            scope = host_scope.clone();
                            eprintln!(
                                "{}",
                                if color {
                                    "\x1b[90m  [scope cleared]\x1b[0m"
                                } else {
                                    "  [scope cleared]"
                                }
                            );
                            continue;
                        }

                        ":scope" | ":vars" => {
                            print_scope(&scope, color);
                            continue;
                        }

                        ":?" | ":help" => {
                            print_repl_help(color);
                            continue;
                        }

                        "" => continue,
                        _ => {}
                    }
                }

                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(trimmed);

                if buf.trim().is_empty() {
                    buf.clear();
                    cont = false;
                    continue;
                }

                let mut parser = Parser::new(&buf, &backends);
                match parser.parse() {
                    Ok(nodes) if nodes.is_empty() => {
                        buf.clear();
                        cont = false;
                    }
                    Ok(nodes) => {
                        // Add the complete (possibly multi-line) expression to history
                        let _ = rl.add_history_entry(&buf);

                        let t0 = Instant::now();
                        match evaluator.eval_document_with_scope(nodes, &mut scope) {
                            Ok(value) => {
                                print_result(&value);
                                if color {
                                    let elapsed = t0.elapsed();
                                    if elapsed.as_millis() < 1000 {
                                        eprintln!("\x1b[2m  {} ms\x1b[0m", elapsed.as_millis());
                                    } else {
                                        eprintln!("\x1b[2m  {:.2} s\x1b[0m", elapsed.as_secs_f64());
                                    }
                                }
                            }
                            Err(e) => eprintln!("{}", fmt_err(&e.to_string(), color)),
                        }
                        buf.clear();
                        cont = false;
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("Unclosed expression") {
                            // Add each partial line to history separately so
                            // the user can recall individual lines if needed.
                            let _ = rl.add_history_entry(trimmed);
                            cont = true;
                        } else {
                            eprintln!("{}", fmt_err(&msg, color));
                            buf.clear();
                            cont = false;
                        }
                    }
                }
            }
        }
    }

    if let Some(ref p) = history_path {
        let _ = rl.save_history(p);
    }

    eprintln!(
        "{}",
        if color {
            "\x1b[90m  bye\x1b[0m"
        } else {
            "  bye"
        }
    );
    Ok(())
}

fn print_scope(scope: &HashMap<String, OValue>, color: bool) {
    if scope.is_empty() {
        eprintln!(
            "{}",
            if color {
                "\x1b[90m  (no bindings)\x1b[0m"
            } else {
                "  (no bindings)"
            }
        );
        return;
    }
    let mut names: Vec<_> = scope.keys().collect();
    names.sort();
    if color {
        eprintln!(
            "\x1b[2m  {} binding{}:\x1b[0m",
            names.len(),
            if names.len() == 1 { "" } else { "s" }
        );
    }
    for name in names {
        let val = &scope[name];
        let preview = preview_value(val, color);
        let badge = if color {
            format!("\x1b[90m[{}]\x1b[0m", val.type_name())
        } else {
            format!("[{}]", val.type_name())
        };
        if color {
            eprintln!("  \x1b[35m${name}\x1b[0m = {preview}  {badge}");
        } else {
            eprintln!("  ${name} = {preview}  {badge}");
        }
    }
}

fn preview_value(val: &OValue, color: bool) -> String {
    let full = format_value(val, color, 0);
    // Flatten newlines and cap at 60 chars for inline display
    let flat: String = full
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .take(60)
        .collect();
    if full.len() > 60 {
        format!("{flat}…")
    } else {
        flat
    }
}

fn print_repl_help(color: bool) {
    let h = if color { "\x1b[1m" } else { "" };
    let r = if color { "\x1b[0m" } else { "" };
    let d = if color { "\x1b[90m" } else { "" };
    eprintln!();
    eprintln!("  {h}:q{r} / {h}:quit{r}   {d}exit the REPL{r}");
    eprintln!("  {h}:r{r} / {h}:reset{r}  {d}clear all let-bindings from scope{r}");
    eprintln!("  {h}:?{r} / {h}:help{r}   {d}show this message{r}");
    eprintln!();
    eprintln!("  {d}Multi-line expressions are accepted — keep typing until{r}");
    eprintln!("  {d}the expression closes (the prompt changes to `...`):{r}");
    eprintln!();
    eprintln!("  {h}python^({r}");
    eprintln!("  {h}  2 + 2{r}");
    eprintln!("  {h})_python{r}");
    eprintln!();
}

fn fmt_err(msg: &str, color: bool) -> String {
    if color {
        format!("\x1b[31merror:\x1b[0m {msg}")
    } else {
        format!("error: {msg}")
    }
}

// ─── Value display ────────────────────────────────────────────────────────────

/// Print an OValue to stdout with ANSI color when the terminal supports it.
/// Strings and HTML are emitted raw. Structured values get a dim type badge.
fn print_result(value: &OValue) {
    let color = io::stdout().is_terminal();

    match value {
        OValue::Text { v } => print!("{}", v.utf8),
        OValue::Html { v } => print!("{v}"),
        OValue::Null => {
            println!("{}", if color { "\x1b[2mnull\x1b[0m" } else { "null" });
        }
        OValue::Bool { v } => println!("{}", colored(v, "\x1b[33m", color)),
        OValue::List { v } => println!("{}", format_list(v, color, 0)),
        OValue::Map { v } => println!("{}", format_map(v, color, 0)),
        other => {
            let t = other.type_name();
            let d = format!("{other}");
            if color {
                println!("\x1b[90m[{t}]\x1b[0m {d}")
            } else {
                println!("[{t}] {d}")
            }
        }
    }
}

fn colored(v: &dyn std::fmt::Display, code: &str, color: bool) -> String {
    if color {
        format!("{code}{v}\x1b[0m")
    } else {
        v.to_string()
    }
}

fn format_list(items: &[OValue], color: bool, depth: usize) -> String {
    if items.is_empty() {
        return if color {
            "\x1b[90m[]\x1b[0m".into()
        } else {
            "[]".into()
        };
    }
    let indent = "  ".repeat(depth + 1);
    let close = "  ".repeat(depth);
    let (open_b, close_b) = if color {
        ("\x1b[90m[\x1b[0m", "\x1b[90m]\x1b[0m")
    } else {
        ("[", "]")
    };
    let mut out = format!("{open_b}\n");
    for item in items {
        out.push_str(&indent);
        out.push_str(&format_value(item, color, depth + 1));
        out.push_str(",\n");
    }
    out.push_str(&close);
    out.push_str(close_b);
    out
}

fn format_map(map: &HashMap<String, OValue>, color: bool, depth: usize) -> String {
    if map.is_empty() {
        return if color {
            "\x1b[90m{}\x1b[0m".into()
        } else {
            "{}".into()
        };
    }
    let indent = "  ".repeat(depth + 1);
    let close = "  ".repeat(depth);
    let (open_b, close_b) = if color {
        ("\x1b[90m{\x1b[0m", "\x1b[90m}\x1b[0m")
    } else {
        ("{", "}")
    };
    let mut pairs: Vec<_> = map.iter().collect();
    pairs.sort_by_key(|(k, _)| k.as_str());
    let mut out = format!("{open_b}\n");
    for (k, v) in pairs {
        out.push_str(&indent);
        if color {
            out.push_str(&format!("\x1b[35m\"{k}\"\x1b[0m: "))
        } else {
            out.push_str(&format!("{k:?}: "))
        }
        out.push_str(&format_value(v, color, depth + 1));
        out.push_str(",\n");
    }
    out.push_str(&close);
    out.push_str(close_b);
    out
}

fn format_value(v: &OValue, color: bool, depth: usize) -> String {
    match v {
        OValue::Null => {
            if color {
                "\x1b[2mnull\x1b[0m".into()
            } else {
                "null".into()
            }
        }
        OValue::Bool { v } => colored(v, "\x1b[33m", color),
        OValue::Text { v } => {
            if color {
                format!("\x1b[32m{:?}\x1b[0m", v.utf8)
            } else {
                format!("{:?}", v.utf8)
            }
        }
        OValue::Html { v } => {
            if color {
                format!("\x1b[32m{v:?}\x1b[0m")
            } else {
                format!("{v:?}")
            }
        }
        OValue::List { v } => format_list(v, color, depth),
        OValue::Map { v } => format_map(v, color, depth),
        other => {
            let t = other.type_name();
            let d = format!("{other}");
            if color {
                format!("\x1b[90m[{t}]\x1b[0m {d}")
            } else {
                format!("[{t}] {d}")
            }
        }
    }
}

// ─── Shared backend list ──────────────────────────────────────────────────────

fn registered_backends() -> HashSet<String> {
    // Single source of truth: the central BackendRegistry owns the set of
    // accepted parser tags (canonical names plus aliases).
    o_lang::ir::BackendRegistry::global().registered_backend_tags()
}
