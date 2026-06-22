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
use o_lang::value::OValue;

fn main() -> Result<()> {
    let mut args = env::args().skip(1).collect::<VecDeque<_>>();
    let backends = registered_backends();
    let mut backend_grants = Vec::new();
    while args.front().is_some_and(|arg| arg == "--backend-grant") {
        args.pop_front();
        backend_grants.push(
            args.pop_front()
                .context("--backend-grant requires NAME=LANG[:RIGHT,...]")?,
        );
    }

    // No args in an interactive terminal → REPL.
    // In non-interactive contexts, missing args is a usage error so shell tests
    // and scripts do not silently enter and exit the REPL.
    match args.front().map(String::as_str) {
        Some("--help") | Some("-h") => {
            print_usage(&mut io::stdout())?;
            return Ok(());
        }
        None if io::stdin().is_terminal() && io::stderr().is_terminal() => {
            return run_repl(PathBuf::from("backends"), backends, &backend_grants);
        }
        None => {
            print_usage(&mut io::stderr())?;
            bail!("missing input file (pass a .O file or use --repl)");
        }
        Some("--repl") | Some("-i") => {
            args.pop_front();
            let shim_dir = args
                .pop_front()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("backends"));
            if let Some(extra) = args.pop_front() {
                print_usage(&mut io::stderr())?;
                bail!("unexpected extra argument after --repl: {}", extra);
            }
            return run_repl(shim_dir, backends, &backend_grants);
        }
        _ => {}
    }

    let input_path = args.pop_front().unwrap();
    let shim_dir = args
        .pop_front()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("backends"));
    if let Some(extra) = args.pop_front() {
        print_usage(&mut io::stderr())?;
        bail!("unexpected extra argument: {}", extra);
    }

    let mut source = fs::read_to_string(&input_path)
        .with_context(|| format!("failed to read input file: {}", input_path))?;

    if source.starts_with("#!") {
        source = source
            .find('\n')
            .map(|nl| source[nl + 1..].to_string())
            .unwrap_or_default();
    }

    let start = Instant::now();
    let mut parser = Parser::new(&source, &backends);
    let nodes = parser.parse().context("failed to parse .O source")?;

    let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends);
    let mut scope = HashMap::new();
    for grant in &backend_grants {
        evaluator.install_backend_grant(grant, &mut scope)?;
    }
    let result = evaluator
        .eval_document_with_scope(nodes, &mut scope)
        .context("failed to evaluate .O document")?;

    let elapsed = start.elapsed();
    print_result(&result);

    if io::stderr().is_terminal() {
        if elapsed.as_millis() < 1000 {
            eprintln!("\x1b[2m  {} ms\x1b[0m", elapsed.as_millis());
        } else {
            eprintln!("\x1b[2m  {:.2} s\x1b[0m", elapsed.as_secs_f64());
        }
    }

    Ok(())
}

fn print_usage(out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "Usage:")?;
    writeln!(out, "  O <input.O> [backends_dir]")?;
    writeln!(out, "  O --repl [backends_dir]")?;
    writeln!(
        out,
        "  O --backend-grant NAME=LANG[:RIGHT,...] <input.O> [backends_dir]"
    )?;
    writeln!(out, "  O --help")?;
    writeln!(out)?;
    writeln!(out, "Runs a .O file or starts the interactive REPL.")?;
    writeln!(
        out,
        "With no arguments in an interactive terminal, O starts the REPL."
    )?;
    Ok(())
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
        OValue::Str { v } => print!("{v}"),
        OValue::Html { v } => print!("{v}"),
        OValue::Null => {
            println!("{}", if color { "\x1b[2mnull\x1b[0m" } else { "null" });
        }
        OValue::Bool { v } => println!("{}", colored(v, "\x1b[33m", color)),
        OValue::Int { v } => println!("{}", colored(v, "\x1b[36m", color)),
        OValue::Float { v } => println!("{}", colored(v, "\x1b[36m", color)),
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
        OValue::Int { v } => colored(v, "\x1b[36m", color),
        OValue::Float { v } => colored(v, "\x1b[36m", color),
        OValue::Str { v } => {
            if color {
                format!("\x1b[32m{v:?}\x1b[0m")
            } else {
                format!("{v:?}")
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
    .into_iter()
    .map(String::from)
    .collect()
}
