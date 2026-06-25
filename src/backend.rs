use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use num_bigint::BigInt;
use serde_json::Value;

use crate::value::{FloatFormat, ONumber, OValue, OWireCommand, OWireResponse};
use crate::wire;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn run_backend_from_env_args() -> Result<bool> {
    let mut args = std::env::args();
    let _program = args.next();
    if args.next().as_deref() != Some("--o-backend") {
        return Ok(false);
    }

    let lang = args
        .next()
        .context("--o-backend requires a language name")?;
    if let Some(extra) = args.next() {
        bail!("unexpected argument after --o-backend {lang}: {extra}");
    }

    run_backend(&lang)?;
    Ok(true)
}

pub fn has_native_backend(lang: &str) -> bool {
    matches!(
        lang,
        "bash"
            | "shell"
            | "javascript"
            | "ruby"
            | "rust"
            | "cpp"
            | "java"
            | "nix"
            | "nix_expr"
            | "nix_store"
            | "sql"
            | "haskell"
            | "ocaml"
            | "racket"
            | "lisp"
            | "common_lisp"
            | "csharp"
            | "matlab"
            | "mathematica"
            | "webassembly"
    )
}

pub fn run_backend(lang: &str) -> Result<()> {
    if !has_native_backend(lang) {
        return proxy_legacy_backend(lang);
    }

    let mut backend = RustBackend::default();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    while let Some(command) = wire::read_frame::<_, OWireCommand>(&mut reader)? {
        let response = match command {
            OWireCommand::Exec { code, bindings } => match backend.exec(lang, &code, bindings) {
                Ok(value) => OWireResponse::ok(value),
                Err(error) => OWireResponse::err(format!("{error:#}")),
            },
            OWireCommand::Cleanup => OWireResponse::ok(OValue::Null),
            OWireCommand::Ping => OWireResponse::ok(OValue::Null),
            OWireCommand::EvalResult { .. } => {
                OWireResponse::err("backend received eval_result without a pending eval request")
            }
        };
        wire::write_frame(&mut writer, &response)?;
    }

    Ok(())
}

#[derive(Default)]
struct RustBackend {
    sql: Option<SqlState>,
}

struct SqlState {
    _dir: TempDir,
    db_path: PathBuf,
}

impl RustBackend {
    fn exec(
        &mut self,
        lang: &str,
        code: &str,
        bindings: HashMap<String, OValue>,
    ) -> Result<OValue> {
        match lang {
            "bash" => run_shell("bash", &["-c", code], Some(scalar_env(bindings))),
            "shell" => run_shell("sh", &["-c", code], Some(scalar_env(bindings))),
            "javascript" => run_script("javascript", "js", &javascript_preamble(&bindings), code),
            "ruby" => run_script("ruby", "rb", &ruby_preamble(&bindings), code),
            "rust" => run_rust(code),
            "cpp" => run_cpp(code),
            "java" => run_java(code),
            "nix" | "nix_expr" => run_nix(code),
            "nix_store" => run_nix_store(code),
            "sql" => self.run_sql(code),
            "haskell" => run_haskell(code),
            "ocaml" => run_ocaml(code),
            "racket" => run_file_command("racket", "rkt", code, "racket", &["{file}"]),
            "lisp" | "common_lisp" => run_common_lisp(code),
            "csharp" => run_csharp(code),
            "matlab" => run_matlab(code),
            "mathematica" => run_mathematica(code),
            "webassembly" => run_webassembly(code),
            other => bail!("backend `{other}` is not implemented by the Rust backend runner"),
        }
    }

    fn run_sql(&mut self, code: &str) -> Result<OValue> {
        let code = code.trim();
        if code.is_empty() {
            return Ok(OValue::Null);
        }

        let state = self.sql_state()?;
        let output = Command::new("sqlite3")
            .arg("-batch")
            .arg("-json")
            .arg(&state.db_path)
            .arg(code)
            .output()
            .context("sqlite3 is not installed or not in PATH")?;
        expect_success("sqlite3 execution failed", output.clone())?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            if sql_has_query_result(code) {
                return Ok(OValue::list(Vec::new()));
            }
            return Ok(OValue::str_("Statement executed successfully"));
        }

        let json: Value = serde_json::from_str(trimmed).context("sqlite3 returned non-JSON")?;
        sqlite_json_to_ovalue(json)
    }

    fn sql_state(&mut self) -> Result<&SqlState> {
        if self.sql.is_none() {
            let dir = TempDir::new("o-backend-sql")?;
            let db_path = dir.path().join("state.sqlite3");
            self.sql = Some(SqlState { _dir: dir, db_path });
        }
        Ok(self.sql.as_ref().expect("sql state was just initialized"))
    }
}

fn proxy_legacy_backend(lang: &str) -> Result<()> {
    let shim = std::env::var_os("O_BACKEND_LEGACY_SHIM")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("backend `{lang}` has no Rust adapter and no legacy shim path"))?;
    if !shim.exists() {
        bail!(
            "backend `{lang}` has no Rust adapter and legacy shim does not exist: {}",
            shim.display()
        );
    }

    let python =
        which::which("python3").context("python3 is required for legacy backend bridge")?;
    let mut child = Command::new(python)
        .arg(&shim)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to spawn legacy backend shim: {}", shim.display()))?;

    let mut child_stdin = child
        .stdin
        .take()
        .context("legacy backend did not provide stdin")?;
    let mut child_stdout = child
        .stdout
        .take()
        .context("legacy backend did not provide stdout")?;

    let stdin_thread = thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let _ = copy_and_flush(&mut stdin, &mut child_stdin);
    });
    let stdout_thread = thread::spawn(move || {
        let mut stdout = io::stdout().lock();
        let _ = copy_and_flush(&mut child_stdout, &mut stdout);
    });

    let status = child.wait()?;
    let _ = stdin_thread.join();
    let _ = stdout_thread.join();
    if !status.success() {
        bail!("legacy backend shim exited with status {status}");
    }
    Ok(())
}

fn copy_and_flush<R, W>(reader: &mut R, writer: &mut W) -> io::Result<u64>
where
    R: Read,
    W: Write,
{
    let mut total = 0;
    let mut buffer = [0_u8; 8192];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            writer.flush()?;
            return Ok(total);
        }
        writer.write_all(&buffer[..n])?;
        writer.flush()?;
        total += n as u64;
    }
}

fn run_shell(program: &str, args: &[&str], env: Option<HashMap<String, String>>) -> Result<OValue> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(env) = env {
        command.envs(env);
    }
    output_to_value(
        program,
        command
            .output()
            .with_context(|| format!("{program} is not installed or not in PATH"))?,
    )
}

fn run_script(lang: &str, suffix: &str, preamble: &str, code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-script")?;
    let source = temp.path().join(format!("main.{suffix}"));
    fs::write(&source, format!("{preamble}{code}"))?;
    let program = match lang {
        "javascript" => "node",
        "ruby" => "ruby",
        _ => lang,
    };
    output_to_value(
        program,
        Command::new(program)
            .arg(&source)
            .output()
            .with_context(|| format!("{program} is not installed or not in PATH"))?,
    )
}

fn run_file_command(
    label: &str,
    suffix: &str,
    code: &str,
    program: &str,
    args: &[&str],
) -> Result<OValue> {
    let temp = TempDir::new("o-backend-file")?;
    let source = temp.path().join(format!("main.{suffix}"));
    fs::write(&source, code)?;
    let source_text = source.to_string_lossy();
    let mut command = Command::new(program);
    for arg in args {
        if *arg == "{file}" {
            command.arg(source_text.as_ref());
        } else {
            command.arg(arg);
        }
    }
    output_to_value(
        label,
        command
            .output()
            .with_context(|| format!("{program} is not installed or not in PATH"))?,
    )
}

fn run_rust(code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-rust")?;
    let source = temp.path().join("main.rs");
    let binary = temp.path().join("main");
    fs::write(&source, code)?;
    expect_success(
        "rustc compilation failed",
        Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .context("rustc is not installed or not in PATH")?,
    )?;
    output_to_value(
        "rust program",
        Command::new(&binary)
            .output()
            .context("failed to execute compiled Rust program")?,
    )
}

fn run_cpp(code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-cpp")?;
    let source = temp.path().join("main.cpp");
    let binary = temp.path().join("main");
    fs::write(&source, code)?;
    expect_success(
        "g++ compilation failed",
        Command::new("g++")
            .arg("-std=c++17")
            .arg("-o")
            .arg(&binary)
            .arg(&source)
            .output()
            .context("g++ is not installed or not in PATH")?,
    )?;
    output_to_value(
        "C++ program",
        Command::new(&binary)
            .output()
            .context("failed to execute compiled C++ program")?,
    )
}

fn run_java(code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-java")?;
    let class_name = java_class_name(code);
    let source = temp.path().join(format!("{class_name}.java"));
    fs::write(&source, code)?;
    expect_success(
        "javac compilation failed",
        Command::new("javac")
            .arg(&source)
            .output()
            .context("javac is not installed or not in PATH")?,
    )?;
    output_to_value(
        "java",
        Command::new("java")
            .arg("-cp")
            .arg(temp.path())
            .arg(class_name)
            .output()
            .context("java is not installed or not in PATH")?,
    )
}

fn run_nix(code: &str) -> Result<OValue> {
    let output = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "eval",
            "--json",
            "--impure",
            "--expr",
            code,
        ])
        .output()
        .context("nix is not installed or not in PATH")?;
    expect_success("nix eval failed", output.clone())?;
    let json: Value =
        serde_json::from_slice(&output.stdout).context("nix eval returned non-JSON")?;
    json_value_to_ovalue(json)
}

fn run_nix_store(code: &str) -> Result<OValue> {
    let output = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "eval",
            "--raw",
            "--impure",
            "--expr",
            code,
        ])
        .output()
        .context("nix is not installed or not in PATH")?;
    expect_success("nix eval --raw failed", output.clone())?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !path.starts_with("/nix/store/") {
        bail!("expression did not evaluate to a Nix store path: {path:?}");
    }
    Ok(OValue::store_path(path))
}

fn run_haskell(code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-haskell")?;
    let source = temp.path().join("Main.hs");
    fs::write(&source, code)?;
    if which::which("runghc").is_ok() {
        return output_to_value(
            "Haskell",
            Command::new("runghc")
                .arg(&source)
                .output()
                .context("failed to execute runghc")?,
        );
    }
    if which::which("ghc").is_ok() {
        let binary = temp.path().join("Main");
        expect_success(
            "ghc compilation failed",
            Command::new("ghc")
                .arg("-o")
                .arg(&binary)
                .arg(&source)
                .output()
                .context("failed to execute ghc")?,
        )?;
        return output_to_value(
            "Haskell",
            Command::new(&binary)
                .output()
                .context("failed to execute compiled Haskell program")?,
        );
    }
    bail!("Neither runghc nor ghc found in PATH. Install GHC.")
}

fn run_ocaml(code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-ocaml")?;
    let source = temp.path().join("main.ml");
    fs::write(&source, code)?;
    if which::which("ocaml").is_ok() {
        return output_to_value(
            "OCaml",
            Command::new("ocaml")
                .arg(&source)
                .output()
                .context("failed to execute ocaml")?,
        );
    }
    let compiler = if which::which("ocamlopt").is_ok() {
        Some("ocamlopt")
    } else if which::which("ocamlc").is_ok() {
        Some("ocamlc")
    } else {
        None
    };
    let Some(compiler) = compiler else {
        bail!("ocaml is not installed or not in PATH");
    };
    let binary = temp.path().join("main");
    expect_success(
        format!("{compiler} compilation failed"),
        Command::new(compiler)
            .arg("-o")
            .arg(&binary)
            .arg(&source)
            .output()
            .with_context(|| format!("failed to execute {compiler}"))?,
    )?;
    output_to_value(
        "OCaml",
        Command::new(&binary)
            .output()
            .context("failed to execute compiled OCaml program")?,
    )
}

fn run_common_lisp(code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-lisp")?;
    let source = temp.path().join("main.lisp");
    fs::write(&source, code)?;
    if which::which("sbcl").is_ok() {
        return output_to_value(
            "Common Lisp",
            Command::new("sbcl")
                .arg("--script")
                .arg(&source)
                .output()
                .context("failed to execute sbcl")?,
        );
    }
    if which::which("clisp").is_ok() {
        return output_to_value(
            "Common Lisp",
            Command::new("clisp")
                .arg(&source)
                .output()
                .context("failed to execute clisp")?,
        );
    }
    bail!("Neither sbcl nor clisp found in PATH. Install a Common Lisp runtime.")
}

fn run_csharp(code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-csharp")?;
    if which::which("dotnet").is_ok() {
        expect_success(
            "dotnet project creation failed",
            Command::new("dotnet")
                .args(["new", "console", "--force", "-o"])
                .arg(temp.path())
                .output()
                .context("failed to execute dotnet")?,
        )?;
        fs::write(temp.path().join("Program.cs"), code)?;
        return output_to_value(
            "C#",
            Command::new("dotnet")
                .arg("run")
                .arg("--project")
                .arg(temp.path())
                .output()
                .context("failed to execute dotnet run")?,
        );
    }
    if which::which("mcs").is_ok() && which::which("mono").is_ok() {
        let source = temp.path().join("Program.cs");
        let binary = temp.path().join("Program.exe");
        fs::write(&source, code)?;
        expect_success(
            "mcs compilation failed",
            Command::new("mcs")
                .arg(format!("-out:{}", binary.display()))
                .arg(&source)
                .output()
                .context("failed to execute mcs")?,
        )?;
        return output_to_value(
            "C#",
            Command::new("mono")
                .arg(&binary)
                .output()
                .context("failed to execute mono")?,
        );
    }
    bail!("No C# compiler found. Install .NET SDK (dotnet) or Mono (mcs/mono).")
}

fn run_matlab(code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-matlab")?;
    let source = temp.path().join("script.m");
    fs::write(&source, code)?;
    if which::which("octave").is_ok() {
        return output_to_value(
            "MATLAB/Octave",
            Command::new("octave")
                .args(["--no-gui", "--norc", "--silent"])
                .arg(&source)
                .output()
                .context("failed to execute octave")?,
        );
    }
    if which::which("matlab").is_ok() {
        let script_dir = temp.path().to_string_lossy();
        return output_to_value(
            "MATLAB",
            Command::new("matlab")
                .arg("-batch")
                .arg(format!("addpath('{script_dir}'); script"))
                .output()
                .context("failed to execute matlab")?,
        );
    }
    bail!("Neither GNU Octave nor MATLAB found in PATH.")
}

fn run_mathematica(code: &str) -> Result<OValue> {
    run_file_command(
        "Mathematica",
        "wls",
        code,
        "wolframscript",
        &["-file", "{file}"],
    )
}

fn run_webassembly(code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-wasm")?;
    let wasm = temp.path().join("module.wasm");
    if code.trim_start().starts_with("(module") || code.trim_start().starts_with("(func") {
        let wat = temp.path().join("module.wat");
        fs::write(&wat, code)?;
        expect_success(
            "wat2wasm failed",
            Command::new("wat2wasm")
                .arg(&wat)
                .arg("-o")
                .arg(&wasm)
                .output()
                .context("wat2wasm is not installed or not in PATH")?,
        )?;
    } else {
        fs::write(&wasm, code.as_bytes())?;
    }

    if which::which("wasmtime").is_ok() {
        return output_to_value(
            "wasmtime",
            Command::new("wasmtime")
                .arg(&wasm)
                .output()
                .context("failed to execute wasmtime")?,
        );
    }
    if which::which("wasmer").is_ok() {
        return output_to_value(
            "wasmer",
            Command::new("wasmer")
                .arg("run")
                .arg(&wasm)
                .output()
                .context("failed to execute wasmer")?,
        );
    }
    bail!("No WebAssembly runtime found. Install wasmtime or wasmer.")
}

fn output_to_value(label: &str, output: Output) -> Result<OValue> {
    expect_success(format!("{label} exited with failure"), output.clone())?;
    Ok(stdout_to_ovalue(&String::from_utf8_lossy(&output.stdout)))
}

fn expect_success(label: impl AsRef<str>, output: Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let code = output
        .status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string());
    if stdout.trim().is_empty() {
        bail!("{} (code {code})\n{}", label.as_ref(), stderr.trim());
    }
    bail!(
        "{} (code {code})\nSTDERR:\n{}\nSTDOUT:\n{}",
        label.as_ref(),
        stderr.trim(),
        stdout.trim()
    )
}

fn stdout_to_ovalue(output: &str) -> OValue {
    let text = trim_stdout(output);
    let stripped = text.trim();
    if !stripped.is_empty() {
        if let Ok(json) = serde_json::from_str::<Value>(stripped) {
            if let Ok(value) = json_value_to_ovalue(json) {
                return value;
            }
        }

        if is_integer_literal(stripped) {
            if let Ok(int) = stripped.parse::<i64>() {
                return OValue::int(int);
            }
            if let Some(big) = BigInt::parse_bytes(stripped.as_bytes(), 10) {
                return OValue::big_int(big);
            }
        }

        if is_float_literal(stripped) {
            if let Ok(float) = stripped.parse::<f64>() {
                return float_to_ovalue(float);
            }
        }
    }
    OValue::str_(text)
}

fn json_value_to_ovalue(value: Value) -> Result<OValue> {
    Ok(match value {
        Value::Null => OValue::Null,
        Value::Bool(v) => OValue::bool_(v),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                OValue::int(value)
            } else if let Some(value) = number.as_u64() {
                match i64::try_from(value) {
                    Ok(value) => OValue::int(value),
                    Err(_) => OValue::big_int(BigInt::from(value)),
                }
            } else if let Some(value) = number.as_f64() {
                float_to_ovalue(value)
            } else {
                OValue::str_(number.to_string())
            }
        }
        Value::String(v) => OValue::str_(v),
        Value::Array(values) => OValue::list(
            values
                .into_iter()
                .map(json_value_to_ovalue)
                .collect::<Result<Vec<_>>>()?,
        ),
        Value::Object(map) => {
            let tagged = map
                .get("t")
                .and_then(Value::as_str)
                .is_some_and(|tag| !tag.is_empty());
            let value = Value::Object(map);
            if tagged {
                serde_json::from_value(value).context("tagged JSON is not a valid OValue")?
            } else if let Value::Object(map) = value {
                OValue::map(
                    map.into_iter()
                        .map(|(key, value)| Ok((key, json_value_to_ovalue(value)?)))
                        .collect::<Result<HashMap<_, _>>>()?,
                )
            } else {
                unreachable!()
            }
        }
    })
}

fn sqlite_json_to_ovalue(value: Value) -> Result<OValue> {
    let Value::Array(rows) = value else {
        return json_value_to_ovalue(value);
    };
    if rows.len() == 1 {
        if let Some(object) = rows[0].as_object() {
            if object.len() == 1 {
                if let Some((_, value)) = object.iter().next() {
                    return json_value_to_ovalue(value.clone());
                }
            }
        }
    }
    json_value_to_ovalue(Value::Array(rows))
}

fn sql_has_query_result(code: &str) -> bool {
    code.split(';')
        .map(str::trim)
        .filter(|stmt| !stmt.is_empty())
        .next_back()
        .is_some_and(|stmt| {
            let upper = stmt
                .chars()
                .take_while(|ch| !ch.is_whitespace() && *ch != '(')
                .collect::<String>()
                .to_ascii_uppercase();
            matches!(upper.as_str(), "SELECT" | "WITH" | "PRAGMA")
        })
}

fn float_to_ovalue(value: f64) -> OValue {
    if value.is_finite() {
        OValue::float(value)
    } else {
        OValue::number(ONumber::BinaryFloat {
            format: FloatFormat::F64,
            bits: value.to_bits().to_be_bytes().to_vec(),
        })
    }
}

fn trim_stdout(output: &str) -> String {
    let mut text = output.to_string();
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

fn is_integer_literal(value: &str) -> bool {
    let rest = value
        .strip_prefix('+')
        .or_else(|| value.strip_prefix('-'))
        .unwrap_or(value);
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

fn is_float_literal(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    (lower.contains('.') || lower.contains('e')) && lower.parse::<f64>().is_ok()
}

fn scalar_env(bindings: HashMap<String, OValue>) -> HashMap<String, String> {
    bindings
        .into_iter()
        .filter_map(|(name, value)| scalar_string(&value).map(|value| (name, value)))
        .collect()
}

fn scalar_string(value: &OValue) -> Option<String> {
    match value {
        OValue::Str { v }
        | OValue::Text {
            v: crate::value::OText { utf8: v, .. },
        } => Some(v.clone()),
        OValue::Int { v } => Some(v.to_string()),
        OValue::Float { v } => Some(v.to_string()),
        OValue::Number {
            v: ONumber::Int { v },
        } => Some(v.to_string()),
        OValue::Bool { v } => Some(v.to_string()),
        _ => None,
    }
}

fn javascript_preamble(bindings: &HashMap<String, OValue>) -> String {
    let mut preamble = String::new();
    for (name, value) in bindings {
        if !is_identifier(name) {
            continue;
        }
        match value {
            OValue::Str { v } => {
                preamble.push_str(&format!(
                    "const {name} = {};\n",
                    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
                ));
            }
            OValue::Int { v } => preamble.push_str(&format!("const {name} = {v};\n")),
            OValue::Float { v } if v.is_finite() => {
                preamble.push_str(&format!("const {name} = {v};\n"));
            }
            OValue::Bool { v } => {
                preamble.push_str(&format!(
                    "const {name} = {};\n",
                    if *v { "true" } else { "false" }
                ));
            }
            OValue::Null => preamble.push_str(&format!("const {name} = null;\n")),
            OValue::List { v } => push_json_const(&mut preamble, name, v),
            OValue::Map { v } => push_json_const(&mut preamble, name, v),
            _ => {}
        }
    }
    preamble
}

fn push_json_const<T: serde::Serialize>(preamble: &mut String, name: &str, value: &T) {
    if let Ok(json) = serde_json::to_string(value) {
        preamble.push_str(&format!("const {name} = {json};\n"));
    }
}

fn ruby_preamble(bindings: &HashMap<String, OValue>) -> String {
    let mut preamble = String::new();
    for (name, value) in bindings {
        if !is_identifier(name) {
            continue;
        }
        match value {
            OValue::Str { v } => {
                preamble.push_str(&format!(
                    "{name} = {}\n",
                    serde_json::to_string(v).unwrap_or_else(|_| "nil".to_string())
                ));
            }
            OValue::Int { v } => preamble.push_str(&format!("{name} = {v}\n")),
            OValue::Float { v } if v.is_finite() => preamble.push_str(&format!("{name} = {v}\n")),
            OValue::Bool { v } => {
                preamble.push_str(&format!("{name} = {}\n", if *v { "true" } else { "false" }));
            }
            OValue::Null => preamble.push_str(&format!("{name} = nil\n")),
            _ => {}
        }
    }
    preamble
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn java_class_name(code: &str) -> String {
    find_class_after(code, "public class")
        .or_else(|| find_class_after(code, "class"))
        .unwrap_or_else(|| "Main".to_string())
}

fn find_class_after(code: &str, marker: &str) -> Option<String> {
    let idx = code.find(marker)?;
    let after = &code[idx + marker.len()..];
    let name = after.split_whitespace().next()?;
    let name = name
        .chars()
        .take_while(|ch| *ch == '_' || ch.is_ascii_alphanumeric())
        .collect::<String>();
    (!name.is_empty()).then_some(name)
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Result<Self> {
        let base = std::env::temp_dir();
        for _ in 0..100 {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("{prefix}-{}-{now}-{counter}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("failed to create backend temp dir"),
            }
        }
        bail!("failed to create unique backend temp dir")
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
