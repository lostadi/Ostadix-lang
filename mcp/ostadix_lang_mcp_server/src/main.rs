//! Ostadix-lang / O-lang MCP server (Rust-only, stdio).
//!
//! Exposes toolchain helpers so agents can run `O` / `olangc` with a correct
//! backends path — avoiding the relative-`backends` and `$VAR` splice traps.
//!
//! Logging goes to **stderr** only (stdout is MCP JSON-RPC).

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

#[derive(Clone)]
struct OstadixMcp {
    tool_router: ToolRouter<Self>,
}

impl OstadixMcp {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Users/ustad"))
}

fn resolve_lang_root() -> PathBuf {
    if let Ok(p) = std::env::var("O_LANG_ROOT") {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            return pb;
        }
    }
    let candidates = [
        home_dir().join("Ostadix-lang"),
        home_dir().join("O-lang"),
        PathBuf::from("/Users/ustad/Ostadix-lang"),
    ];
    for c in candidates {
        if c.is_dir() {
            return c;
        }
    }
    home_dir().join("Ostadix-lang")
}

fn resolve_backends(root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("O_BACKENDS_DIR") {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            return pb;
        }
    }
    root.join("backends")
}

fn resolve_o_bin(root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("OLANG") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return pb;
        }
    }
    let release = root.join("target/release/O");
    if release.is_file() {
        return release;
    }
    which::which("O").unwrap_or_else(|_| PathBuf::from("O"))
}

fn resolve_olangc(root: &Path) -> PathBuf {
    let release = root.join("target/release/olangc");
    if release.is_file() {
        return release;
    }
    which::which("olangc").unwrap_or_else(|_| PathBuf::from("olangc"))
}

fn text_ok(s: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(s.into())]))
}

fn text_err(s: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![Content::text(s.into())]))
}

async fn run_cmd(
    program: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(&str, String)],
    timeout_secs: u64,
) -> Result<(i32, String, String), String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // On timeout below, the wait_with_output() future (and the Child it
        // owns) is dropped without ever calling .kill() explicitly. Without
        // this, the process is orphaned rather than reaped.
        .kill_on_drop(true);
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", program.display()))?;
    let out = tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
        .await
        .map_err(|_| format!("timeout after {timeout_secs}s"))?
        .map_err(|e| format!("wait: {e}"))?;
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok((code, stdout, stderr))
}

fn format_run(code: i32, stdout: &str, stderr: &str) -> String {
    let mut s = format!("exit={code}\n");
    if !stdout.is_empty() {
        s.push_str("--- stdout ---\n");
        s.push_str(stdout);
        if !stdout.ends_with('\n') {
            s.push('\n');
        }
    }
    if !stderr.is_empty() {
        s.push_str("--- stderr ---\n");
        s.push_str(stderr);
        if !stderr.ends_with('\n') {
            s.push('\n');
        }
    }
    s
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RunOArgs {
    #[schemars(description = "Path to a .O program (absolute or relative to cwd)")]
    path: String,
    #[schemars(description = "Optional working directory")]
    cwd: Option<String>,
    #[schemars(description = "Timeout seconds (default 120)")]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OlangcArgs {
    #[schemars(description = "Path to a .O program")]
    path: String,
    #[schemars(
        description = "olangc target: ir | dot | script | wasm | or omit for default AOT analysis"
    )]
    target: Option<String>,
    #[schemars(description = "Optional -o output path")]
    output: Option<String>,
    #[schemars(description = "Timeout seconds (default 180)")]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchRunArgs {
    #[schemars(
        description = "Search tool name without .O, e.g. sptm_retype_catalog, nscramble_mine, lab_pipeline"
    )]
    name: String,
    #[schemars(description = "a18re work root (default A18_WORK or ~/a18re)")]
    work: Option<String>,
    #[schemars(description = "Timeout seconds (default 300)")]
    timeout_secs: Option<u64>,
}

#[tool_router]
impl OstadixMcp {
    #[tool(
        description = "Report O-lang / Ostadix-lang environment: O_LANG_ROOT, backends, O/olangc paths, shim presence"
    )]
    async fn o_env(&self) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let o_bin = resolve_o_bin(&root);
        let olangc = resolve_olangc(&root);
        let shim = backends.join("python_shim.py");
        let msg = format!(
            "O_LANG_ROOT={}\nO_BACKENDS_DIR={}\nO_bin={}\nolangc={}\npython_shim={} ({})\nnote=always pass absolute backends dir to O; never bare \"backends\" from unrelated cwd; never put $VAR inside .O sources\n",
            root.display(),
            backends.display(),
            o_bin.display(),
            olangc.display(),
            shim.display(),
            if shim.is_file() { "ok" } else { "MISSING" }
        );
        text_ok(msg)
    }

    #[tool(
        description = "Smoke-test O toolchain: run examples/hello.O (expect 2) with correct backends path"
    )]
    async fn o_smoke(&self) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let o_bin = resolve_o_bin(&root);
        let hello = root.join("examples/hello.O");
        if !hello.is_file() {
            return text_err(format!("missing {}", hello.display()));
        }
        if !backends.join("python_shim.py").is_file() {
            return text_err(format!(
                "backends invalid: {} (no python_shim.py)",
                backends.display()
            ));
        }
        match run_cmd(
            &o_bin,
            &[hello.to_str().unwrap_or(""), backends.to_str().unwrap_or("")],
            Some(&root),
            &[
                ("O_LANG_ROOT", root.display().to_string()),
                ("O_BACKENDS_DIR", backends.display().to_string()),
            ],
            60,
        )
        .await
        {
            Ok((code, stdout, stderr)) => {
                let body = format_run(code, &stdout, &stderr);
                if code == 0 && stdout.contains('2') {
                    text_ok(format!("SMOKE_OK\n{body}"))
                } else {
                    text_err(format!("SMOKE_FAIL\n{body}"))
                }
            }
            Err(e) => text_err(e),
        }
    }

    #[tool(
        description = "Run a .O program with the O interpreter using absolute O_BACKENDS_DIR (fixes relative backends failures)"
    )]
    async fn o_run(
        &self,
        Parameters(args): Parameters<RunOArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let o_bin = resolve_o_bin(&root);
        let path = PathBuf::from(&args.path);
        let cwd = args
            .cwd
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                path.parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            });
        if !path.is_file() {
            return text_err(format!("not a file: {}", path.display()));
        }
        if !backends.is_dir() {
            return text_err(format!("backends missing: {}", backends.display()));
        }
        let timeout = args.timeout_secs.unwrap_or(120);
        match run_cmd(
            &o_bin,
            &[
                path.to_str().unwrap_or(""),
                backends.to_str().unwrap_or(""),
            ],
            Some(&cwd),
            &[
                ("O_LANG_ROOT", root.display().to_string()),
                ("O_BACKENDS_DIR", backends.display().to_string()),
                ("A18_WORK", cwd.display().to_string()),
            ],
            timeout,
        )
        .await
        {
            Ok((code, stdout, stderr)) => {
                let body = format_run(code, &stdout, &stderr);
                if code == 0 {
                    text_ok(body)
                } else {
                    text_err(body)
                }
            }
            Err(e) => text_err(e),
        }
    }

    #[tool(description = "Run olangc on a .O file (targets: ir, dot, script, wasm, or omit)")]
    async fn o_olangc(
        &self,
        Parameters(args): Parameters<OlangcArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let olangc = resolve_olangc(&root);
        let path = PathBuf::from(&args.path);
        if !path.is_file() {
            return text_err(format!("not a file: {}", path.display()));
        }
        let mut argv: Vec<String> = vec![path.display().to_string()];
        if let Some(t) = &args.target {
            argv.push("--target".into());
            argv.push(t.clone());
        }
        if let Some(o) = &args.output {
            argv.push("-o".into());
            argv.push(o.clone());
        }
        argv.push("--shim-dir".into());
        argv.push(backends.display().to_string());
        let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        let timeout = args.timeout_secs.unwrap_or(180);
        match run_cmd(
            &olangc,
            &refs,
            path.parent(),
            &[
                ("O_LANG_ROOT", root.display().to_string()),
                ("O_BACKENDS_DIR", backends.display().to_string()),
            ],
            timeout,
        )
        .await
        {
            Ok((code, stdout, stderr)) => {
                let body = format_run(code, &stdout, &stderr);
                if code == 0 {
                    text_ok(body)
                } else {
                    text_err(body)
                }
            }
            Err(e) => text_err(e),
        }
    }

    #[tool(
        description = "Toolchain doctor: check O, olangc, backends, python_shim, and optional a18re search/o-run"
    )]
    async fn o_doctor(&self) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let o_bin = resolve_o_bin(&root);
        let olangc = resolve_olangc(&root);
        let mut lines = vec![
            format!("O_LANG_ROOT={} exists={}", root.display(), root.is_dir()),
            format!(
                "O_BACKENDS_DIR={} exists={}",
                backends.display(),
                backends.is_dir()
            ),
            format!("O={} exists={}", o_bin.display(), o_bin.is_file()),
            format!("olangc={} exists={}", olangc.display(), olangc.is_file()),
            format!(
                "python_shim={}",
                backends.join("python_shim.py").is_file()
            ),
        ];
        // list a few shims
        if let Ok(rd) = std::fs::read_dir(&backends) {
            let mut shims: Vec<_> = rd
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with("_shim.py"))
                .collect();
            shims.sort();
            lines.push(format!("shims({}): {}", shims.len(), shims.join(", ")));
        }
        let a18 = home_dir().join("a18re");
        lines.push(format!(
            "a18re={} o-run={}",
            a18.is_dir(),
            a18.join("search/o-run").is_file()
        ));
        text_ok(lines.join("\n") + "\n")
    }

    #[tool(
        description = "Run an a18re search tool by name (e.g. sptm_retype_catalog, nscramble_mine, lab_pipeline) with correct backends"
    )]
    async fn o_search_run(
        &self,
        Parameters(args): Parameters<SearchRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let o_bin = resolve_o_bin(&root);
        let work = args
            .work
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("A18_WORK").map(PathBuf::from))
            .unwrap_or_else(|| home_dir().join("a18re"));
        let mut name = args.name.trim().to_string();
        if name.ends_with(".O") {
            name = name.trim_end_matches(".O").to_string();
        }
        let path = if Path::new(&name).is_file() {
            PathBuf::from(&name)
        } else {
            work.join("search").join(format!("{name}.O"))
        };
        if !path.is_file() {
            return text_err(format!(
                "not found: {} (tried search/{}.O under {})",
                args.name,
                name,
                work.display()
            ));
        }
        // Refuse relative backends pitfalls: always pass absolute backends
        let timeout = args.timeout_secs.unwrap_or(300);
        match run_cmd(
            &o_bin,
            &[
                path.to_str().unwrap_or(""),
                backends.to_str().unwrap_or(""),
            ],
            Some(&work),
            &[
                ("O_LANG_ROOT", root.display().to_string()),
                ("O_BACKENDS_DIR", backends.display().to_string()),
                ("A18_WORK", work.display().to_string()),
            ],
            timeout,
        )
        .await
        {
            Ok((code, stdout, stderr)) => {
                let body = format!(
                    "program={}\nbackends={}\nwork={}\n{}",
                    path.display(),
                    backends.display(),
                    work.display(),
                    format_run(code, &stdout, &stderr)
                );
                if code == 0 {
                    text_ok(body)
                } else {
                    text_err(body)
                }
            }
            Err(e) => text_err(e),
        }
    }
}

#[tool_handler]
impl ServerHandler for OstadixMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Ostadix-lang / O-lang MCP (Rust). Use o_env/o_doctor first. \
Always run .O programs via o_run or o_search_run so backends is absolute. \
Never pass the literal string O_BACKENDS_DIR; never put $VAR inside .O sources (O splices $IDENT)."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stderr only — stdout is MCP
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let server = OstadixMcp::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
