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
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
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
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn canonical_directory(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        path.canonicalize().ok()
    } else {
        None
    }
}

fn is_lang_root(path: &Path) -> bool {
    path.join("Cargo.toml").is_file()
        && path.join("backends/python_shim.py").is_file()
        && path.join("examples/hello.O").is_file()
}

fn resolve_lang_root() -> PathBuf {
    if let Some(path) = std::env::var_os("O_LANG_ROOT").map(PathBuf::from) {
        if is_lang_root(&path) {
            return canonical_directory(&path).unwrap_or(path);
        }
    }

    if let Ok(current) = std::env::current_dir() {
        for candidate in current.ancestors() {
            if is_lang_root(candidate) {
                return canonical_directory(candidate).unwrap_or_else(|| candidate.to_path_buf());
            }
        }
    }

    for candidate in [home_dir().join("Ostadix-lang"), home_dir().join("O-lang")] {
        if is_lang_root(&candidate) {
            return canonical_directory(&candidate).unwrap_or(candidate);
        }
    }

    std::env::current_dir().unwrap_or_else(|_| home_dir().join("Ostadix-lang"))
}

fn resolve_backends(root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("O_BACKENDS_DIR") {
        let pb = PathBuf::from(p);
        if let Some(canonical) = canonical_directory(&pb) {
            return canonical;
        }
    }
    let backends = root.join("backends");
    canonical_directory(&backends).unwrap_or(backends)
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

fn append_existing_path(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if candidate.is_dir() && !paths.contains(&candidate) {
        paths.push(candidate);
    }
}

fn append_path_list(paths: &mut Vec<PathBuf>, encoded: Option<&OsStr>) {
    let Some(encoded) = encoded else {
        return;
    };
    for candidate in std::env::split_paths(encoded) {
        if !paths.contains(&candidate) {
            paths.push(candidate);
        }
    }
}

/// Preserve the client's PATH, then append local runtime locations commonly
/// omitted by GUI/MCP launchers. Appending rather than prepending ensures this
/// discovery fallback never silently replaces an explicitly selected runtime.
fn runtime_search_path_from(
    root: &Path,
    home: &Path,
    inherited: Option<&OsStr>,
    explicit_extra: Option<&OsStr>,
) -> OsString {
    let mut paths = Vec::new();
    append_path_list(&mut paths, inherited);
    append_path_list(&mut paths, explicit_extra);

    for candidate in [
        root.join("target/release"),
        home.join(".local/bin"),
        home.join(".cargo/bin"),
        home.join(".nix-profile/bin"),
        home.join(".volta/bin"),
        home.join(".pyenv/shims"),
        home.join(".rbenv/shims"),
        home.join(".asdf/shims"),
        home.join(".local/share/mise/shims"),
        home.join(".local/share/fnm/aliases/default/bin"),
        home.join(".ghcup/bin"),
        home.join(".cabal/bin"),
        home.join(".opam/default/bin"),
        home.join(".dotnet"),
        home.join(".dotnet/tools"),
        home.join(".wasmtime/bin"),
        home.join(".wasmer/bin"),
        home.join(".bun/bin"),
        home.join(".sdkman/candidates/java/current/bin"),
        home.join("miniforge3/bin"),
        home.join("miniconda3/bin"),
        home.join("anaconda3/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/opt/homebrew/opt/openjdk/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/local/sbin"),
        PathBuf::from("/usr/local/opt/openjdk/bin"),
        PathBuf::from("/nix/var/nix/profiles/default/bin"),
        PathBuf::from("/run/current-system/sw/bin"),
        PathBuf::from("/Library/Frameworks/Mono.framework/Versions/Current/Commands"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/sbin"),
    ] {
        append_existing_path(&mut paths, candidate);
    }

    std::env::join_paths(&paths).unwrap_or_else(|_| inherited.unwrap_or_default().to_os_string())
}

fn append_environment_runtime_paths(paths: &mut Vec<PathBuf>) {
    for variable in ["NVM_BIN", "PNPM_HOME"] {
        if let Some(path) = std::env::var_os(variable) {
            append_existing_path(paths, PathBuf::from(path));
        }
    }
    for variable in [
        "CONDA_PREFIX",
        "VIRTUAL_ENV",
        "JAVA_HOME",
        "OPAM_SWITCH_PREFIX",
        "CARGO_HOME",
        "VOLTA_HOME",
        "GEM_HOME",
    ] {
        if let Some(prefix) = std::env::var_os(variable) {
            append_existing_path(paths, PathBuf::from(prefix).join("bin"));
        }
    }
    for variable in ["PYENV_ROOT", "RBENV_ROOT"] {
        if let Some(prefix) = std::env::var_os(variable) {
            append_existing_path(paths, PathBuf::from(prefix).join("shims"));
        }
    }
    if let Some(root) = std::env::var_os("DOTNET_ROOT") {
        append_existing_path(paths, PathBuf::from(root));
    }
}

fn runtime_search_path(root: &Path) -> OsString {
    let inherited = std::env::var_os("PATH");
    let explicit_extra = std::env::var_os("OSTADIX_RUNTIME_PATH");
    let base = runtime_search_path_from(
        root,
        &home_dir(),
        inherited.as_deref(),
        explicit_extra.as_deref(),
    );
    let mut paths = std::env::split_paths(&base).collect::<Vec<_>>();
    append_environment_runtime_paths(&mut paths);
    std::env::join_paths(&paths).unwrap_or(base)
}

#[derive(Clone, Copy)]
struct RuntimeSpec {
    backends: &'static [&'static str],
    alternatives: &'static [&'static [&'static str]],
}

const BUILTIN_BACKENDS: &[&str] = &[
    "O", "quote", "nix_expr", "html", "markdown", "latex", "text",
];

/// Descriptive runtime requirements for every non-builtin canonical backend
/// in `src/ir.rs::BACKEND_SPECS`. This inventory does not authorize execution;
/// the selected backend adapter still validates and launches the actual tool.
const RUNTIME_SPECS: &[RuntimeSpec] = &[
    RuntimeSpec {
        backends: &["python"],
        alternatives: &[&["python3"]],
    },
    RuntimeSpec {
        backends: &["bash"],
        alternatives: &[&["bash"]],
    },
    RuntimeSpec {
        backends: &["shell"],
        alternatives: &[&["sh"]],
    },
    RuntimeSpec {
        backends: &["javascript"],
        alternatives: &[&["node"]],
    },
    RuntimeSpec {
        backends: &["ruby"],
        alternatives: &[&["ruby"]],
    },
    RuntimeSpec {
        backends: &["rust"],
        alternatives: &[&["rustc"]],
    },
    RuntimeSpec {
        backends: &["c"],
        alternatives: &[&["cc"]],
    },
    RuntimeSpec {
        backends: &["cpp"],
        alternatives: &[&["g++"]],
    },
    RuntimeSpec {
        backends: &["java"],
        alternatives: &[&["javac", "java"]],
    },
    RuntimeSpec {
        backends: &["nix", "nix_store"],
        alternatives: &[&["nix"]],
    },
    RuntimeSpec {
        backends: &["nixos_test"],
        alternatives: &[&["python3", "nix"]],
    },
    RuntimeSpec {
        backends: &["sql"],
        alternatives: &[&["sqlite3"]],
    },
    RuntimeSpec {
        backends: &["haskell"],
        alternatives: &[&["runghc"], &["ghc"]],
    },
    RuntimeSpec {
        backends: &["ocaml"],
        alternatives: &[&["ocaml"], &["ocamlopt"], &["ocamlc"]],
    },
    RuntimeSpec {
        backends: &["racket"],
        alternatives: &[&["racket"]],
    },
    RuntimeSpec {
        backends: &["lisp", "common_lisp"],
        alternatives: &[&["sbcl"], &["clisp"]],
    },
    RuntimeSpec {
        backends: &["csharp"],
        alternatives: &[&["dotnet"], &["mcs", "mono"]],
    },
    RuntimeSpec {
        backends: &["matlab"],
        alternatives: &[&["octave"], &["matlab"]],
    },
    RuntimeSpec {
        backends: &["mathematica"],
        alternatives: &[&["wolframscript"]],
    },
    RuntimeSpec {
        backends: &["webassembly"],
        alternatives: &[&["wat2wasm", "wasmtime"], &["wat2wasm", "wasmer"]],
    },
    RuntimeSpec {
        backends: &["ubuntu_vm"],
        alternatives: &[&["python3", "multipass"]],
    },
];

struct RuntimeDiscovery {
    search_path: OsString,
    lines: Vec<String>,
    backend_count: usize,
    builtin_backends: usize,
    located_backends: usize,
    missing_backends: usize,
}

impl RuntimeDiscovery {
    fn summary(&self) -> String {
        format!(
            "runtime-summary backend-count={} builtin-backends={} located-backends={} missing-backends={}",
            self.backend_count,
            self.builtin_backends,
            self.located_backends,
            self.missing_backends
        )
    }

    fn to_text(&self) -> String {
        let mut out = format!(
            "runtime-search-path={}\n{}\n",
            self.search_path.to_string_lossy(),
            self.summary()
        );
        for line in &self.lines {
            writeln!(out, "{line}").expect("writing to a String cannot fail");
        }
        out.push_str(
            "runtime-note discovery is descriptive; missing optional runtimes do not block unrelated backends\n",
        );
        out
    }
}

fn discover_runtimes(root: &Path) -> RuntimeDiscovery {
    let search_path = runtime_search_path(root);
    let cwd = std::env::current_dir().unwrap_or_else(|_| root.to_path_buf());
    let mut lines = vec![format!(
        "runtime backends={} status=builtin selected=ostadix-runtime",
        BUILTIN_BACKENDS.join(",")
    )];
    let mut located_backends = 0;
    let mut missing_backends = 0;

    for spec in RUNTIME_SPECS {
        let selected = spec.alternatives.iter().find_map(|alternative| {
            let resolved = alternative
                .iter()
                .map(|command| {
                    which::which_in(command, Some(&search_path), &cwd)
                        .ok()
                        .map(|path| (*command, path))
                })
                .collect::<Option<Vec<_>>>()?;
            Some(resolved)
        });
        let backend_names = spec.backends.join(",");
        match selected {
            Some(resolved) => {
                located_backends += spec.backends.len();
                let commands = resolved
                    .iter()
                    .map(|(command, _)| *command)
                    .collect::<Vec<_>>()
                    .join("+");
                let paths = resolved
                    .iter()
                    .map(|(command, path)| format!("{command}={}", path.display()))
                    .collect::<Vec<_>>()
                    .join(",");
                lines.push(format!(
                    "runtime backends={backend_names} status=located selected={commands} paths=[{paths}]"
                ));
            }
            None => {
                missing_backends += spec.backends.len();
                let alternatives = spec
                    .alternatives
                    .iter()
                    .map(|alternative| alternative.join("+"))
                    .collect::<Vec<_>>()
                    .join("|");
                lines.push(format!(
                    "runtime backends={backend_names} status=missing alternatives=[{alternatives}]"
                ));
            }
        }
    }

    RuntimeDiscovery {
        search_path,
        lines,
        backend_count: BUILTIN_BACKENDS.len()
            + RUNTIME_SPECS
                .iter()
                .map(|spec| spec.backends.len())
                .sum::<usize>(),
        builtin_backends: BUILTIN_BACKENDS.len(),
        located_backends,
        missing_backends,
    }
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
        // Keep abnormal future cancellation from orphaning the group leader;
        // the explicit timeout path below kills and reaps the whole group.
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", program.display()))?;

    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout was not piped".to_string())?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| "child stderr was not piped".to_string())?;
    #[cfg(unix)]
    let process_group_id = child.id();
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let completed = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            stdout_pipe.read_to_end(&mut stdout_bytes),
            stderr_pipe.read_to_end(&mut stderr_bytes),
        );
        let status = status.map_err(|e| format!("wait: {e}"))?;
        stdout.map_err(|e| format!("read stdout: {e}"))?;
        stderr.map_err(|e| format!("read stderr: {e}"))?;
        Ok::<_, String>(status)
    })
    .await;

    let status = match completed {
        Ok(result) => result?,
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = process_group_id {
                if let Ok(group_id) = i32::try_from(pid) {
                    // SAFETY: the child was placed in a new process group whose
                    // id is the leader pid. Keep that id before waiting so the
                    // group can still be killed after the leader has exited and
                    // descendants are retaining its stdout/stderr pipes.
                    unsafe {
                        libc::kill(-group_id, libc::SIGKILL);
                    }
                }
            }
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill().await;
            }
            let _ = child.wait().await;
            return Err(format!("timeout after {timeout_secs}s"));
        }
    };
    let code = status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
    Ok((code, stdout, stderr))
}

fn resolve_directory(base: &Path, requested: Option<&str>, label: &str) -> Result<PathBuf, String> {
    let candidate = requested
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                base.join(path)
            }
        })
        .unwrap_or_else(|| base.to_path_buf());
    if !candidate.is_dir() {
        return Err(format!(
            "{label} is not a directory: {}",
            candidate.display()
        ));
    }
    candidate
        .canonicalize()
        .map_err(|error| format!("resolve {label} {}: {error}", candidate.display()))
}

fn resolve_file(base: &Path, requested: &str, label: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(requested);
    let candidate = if path.is_absolute() {
        path
    } else {
        base.join(path)
    };
    if !candidate.is_file() {
        return Err(format!("{label} is not a file: {}", candidate.display()));
    }
    candidate
        .canonicalize()
        .map_err(|error| format!("resolve {label} {}: {error}", candidate.display()))
}

fn resolve_run_target(
    root: &Path,
    requested_path: &str,
    requested_cwd: Option<&str>,
) -> Result<(PathBuf, PathBuf), String> {
    let input_path = Path::new(requested_path);
    let cwd = match requested_cwd {
        Some(requested) => resolve_directory(root, Some(requested), "cwd")?,
        None if input_path.is_absolute() => {
            let parent = input_path.parent().ok_or_else(|| {
                format!(
                    "absolute program has no parent directory: {}",
                    input_path.display()
                )
            })?;
            resolve_directory(parent, None, "program cwd")?
        }
        None => resolve_directory(root, None, "cwd")?,
    };
    let program = resolve_file(&cwd, requested_path, "program")?;
    Ok((program, cwd))
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

// Empty args for zero-parameter tools — emits `type: object` so strict MCP
// clients (OpenCode, TS SDK) accept tools/list instead of rejecting `{}`.
#[derive(Debug, Default, Deserialize)]
struct EmptyArgs {}

impl schemars::JsonSchema for EmptyArgs {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EmptyArgs".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Schemars omits an empty object `properties` map. Some
        // strict MCP clients require the keyword even for zero-argument tools,
        // so construct the exact zero-argument object contract explicitly.
        schemars::json_schema!({
            "type": "object",
            "description": "No parameters",
            "properties": {}
        })
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RunOArgs {
    #[schemars(
        description = "Path to a .O program (absolute paths default cwd to their parent; relative paths use cwd/O_LANG_ROOT)"
    )]
    path: String,
    #[serde(default)]
    #[schemars(description = "Optional working directory (relative paths use O_LANG_ROOT)")]
    cwd: Option<String>,
    #[serde(default)]
    #[schemars(description = "Timeout seconds (default 120)")]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OlangcArgs {
    #[schemars(description = "Path to a .O program (relative paths use O_LANG_ROOT)")]
    path: String,
    #[serde(default)]
    #[schemars(
        description = "olangc target: ir | dot | script | wasm | or omit for default AOT analysis"
    )]
    target: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional -o output path (relative paths use O_LANG_ROOT)")]
    output: Option<String>,
    #[serde(default)]
    #[schemars(description = "Timeout seconds (default 180)")]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchRunArgs {
    #[schemars(
        description = "Search tool name without .O, e.g. sptm_retype_catalog, nscramble_mine, lab_pipeline"
    )]
    name: String,
    #[serde(default)]
    #[schemars(description = "a18re work root (default A18_WORK or ~/a18re)")]
    work: Option<String>,
    #[serde(default)]
    #[schemars(description = "Timeout seconds (default 300)")]
    timeout_secs: Option<u64>,
}

#[tool_router]
impl OstadixMcp {
    #[tool(
        description = "Report O-lang / Ostadix-lang environment: roots, tools, shim presence, and all-runtime summary"
    )]
    async fn o_env(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let o_bin = resolve_o_bin(&root);
        let olangc = resolve_olangc(&root);
        let shim = backends.join("python_shim.py");
        let runtimes = discover_runtimes(&root);
        let msg = format!(
            "O_LANG_ROOT={}\nO_BACKENDS_DIR={}\nO_bin={}\nolangc={}\npython_shim={} ({})\n{}\nnote=always pass absolute backends dir to O; never bare \"backends\" from unrelated cwd; never put $VAR inside .O sources\n",
            root.display(),
            backends.display(),
            o_bin.display(),
            olangc.display(),
            shim.display(),
            if shim.is_file() { "ok" } else { "MISSING" },
            runtimes.summary()
        );
        text_ok(msg)
    }

    #[tool(
        description = "Discover executable requirements for every canonical Ostadix backend without blocking on missing optional runtimes"
    )]
    async fn o_runtimes(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        text_ok(discover_runtimes(&root).to_text())
    }

    #[tool(
        description = "Smoke-test O toolchain: run examples/hello.O (expect 2) with correct backends path"
    )]
    async fn o_smoke(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
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
            &[
                hello.to_str().unwrap_or(""),
                backends.to_str().unwrap_or(""),
            ],
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
        let (path, cwd) = match resolve_run_target(&root, &args.path, args.cwd.as_deref()) {
            Ok(target) => target,
            Err(error) => return text_err(error),
        };
        if !backends.is_dir() {
            return text_err(format!("backends missing: {}", backends.display()));
        }
        let timeout = args.timeout_secs.unwrap_or(120);
        match run_cmd(
            &o_bin,
            &[path.to_str().unwrap_or(""), backends.to_str().unwrap_or("")],
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
        let path = match resolve_file(&root, &args.path, "program") {
            Ok(path) => path,
            Err(error) => return text_err(error),
        };
        let mut argv: Vec<String> = vec![path.display().to_string()];
        if let Some(t) = &args.target {
            argv.push("--target".into());
            argv.push(t.clone());
        }
        if let Some(o) = &args.output {
            argv.push("-o".into());
            let output = PathBuf::from(o);
            argv.push(
                if output.is_absolute() {
                    output
                } else {
                    root.join(output)
                }
                .display()
                .to_string(),
            );
        }
        argv.push("--shim-dir".into());
        argv.push(backends.display().to_string());
        let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        let timeout = args.timeout_secs.unwrap_or(180);
        match run_cmd(
            &olangc,
            &refs,
            Some(&root),
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
        description = "Toolchain doctor: check O, olangc, backends, shims, all backend runtimes, and optional a18re search/o-run"
    )]
    async fn o_doctor(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
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
            format!("python_shim={}", backends.join("python_shim.py").is_file()),
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
        lines.push(discover_runtimes(&root).to_text().trim_end().to_string());
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
        let requested_work = args
            .work
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("A18_WORK").map(PathBuf::from))
            .unwrap_or_else(|| home_dir().join("a18re"));
        let requested_work_text = requested_work.to_string_lossy();
        let work = match resolve_directory(&root, Some(&requested_work_text), "work") {
            Ok(path) => path,
            Err(error) => return text_err(error),
        };
        let requested_name = args.name.trim();
        let direct = PathBuf::from(requested_name);
        let direct = if direct.is_absolute() {
            direct
        } else {
            work.join(direct)
        };
        let tool_name = requested_name.trim_end_matches(".O");
        let candidate = if direct.is_file() {
            direct
        } else {
            work.join("search").join(format!("{tool_name}.O"))
        };
        let path = match resolve_file(
            candidate.parent().unwrap_or(&work),
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(""),
            "search program",
        ) {
            Ok(path) => path,
            Err(_) => {
                return text_err(format!(
                    "not found: {} (tried search/{}.O under {})",
                    args.name,
                    tool_name,
                    work.display()
                ))
            }
        };
        // Refuse relative backends pitfalls: always pass absolute backends
        let timeout = args.timeout_secs.unwrap_or(300);
        match run_cmd(
            &o_bin,
            &[path.to_str().unwrap_or(""), backends.to_str().unwrap_or("")],
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
                "Ostadix-lang / O-lang MCP (Rust). Use o_env/o_runtimes/o_doctor first. \
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
    // GUI/MCP clients often launch with a system-only PATH. Restore known
    // local runtime locations once so discovery and every child backend see
    // the same executable universe.
    let root = resolve_lang_root();
    std::env::set_var("PATH", runtime_search_path(&root));

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

#[cfg(test)]
mod tests {
    use super::{
        is_lang_root, resolve_directory, resolve_file, resolve_run_target, run_cmd,
        runtime_search_path_from, EmptyArgs, BUILTIN_BACKENDS, RUNTIME_SPECS,
    };
    use std::collections::BTreeSet;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is before Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "ostadix-mcp-path-test-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(path.join("backends")).expect("create backends fixture");
            fs::create_dir_all(path.join("examples/space dir")).expect("create examples fixture");
            fs::write(path.join("Cargo.toml"), "[workspace]\n").expect("write Cargo fixture");
            fs::write(path.join("backends/python_shim.py"), "# fixture\n")
                .expect("write shim fixture");
            fs::write(path.join("examples/hello.O"), "text^(ok)_text\n")
                .expect("write hello fixture");
            fs::write(path.join("examples/space dir/demo.O"), "text^(ok)_text\n")
                .expect("write path fixture");
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn recognizes_only_complete_language_roots() {
        let fixture = Fixture::new();
        assert!(is_lang_root(&fixture.0));
        fs::remove_file(fixture.0.join("examples/hello.O")).expect("remove hello fixture");
        assert!(!is_lang_root(&fixture.0));
    }

    #[test]
    fn resolves_relative_cwd_then_program_once() {
        let fixture = Fixture::new();
        let cwd = resolve_directory(&fixture.0, Some("examples/space dir"), "cwd")
            .expect("resolve relative cwd");
        let program = resolve_file(&cwd, "demo.O", "program").expect("resolve relative program");
        assert_eq!(
            program,
            fixture
                .0
                .join("examples/space dir/demo.O")
                .canonicalize()
                .expect("canonical fixture program")
        );
    }

    #[test]
    fn missing_program_reports_effective_absolute_candidate() {
        let fixture = Fixture::new();
        let error = resolve_file(&fixture.0, "examples/missing.O", "program")
            .expect_err("missing program must fail");
        assert!(error.contains(&fixture.0.display().to_string()));
        assert!(error.contains("examples/missing.O"));
    }

    #[test]
    fn absolute_run_path_without_cwd_uses_program_parent() {
        let fixture = Fixture::new();
        let requested = fixture.0.join("examples/space dir/demo.O");
        let (program, cwd) = resolve_run_target(
            &fixture.0,
            requested.to_str().expect("fixture path is UTF-8"),
            None,
        )
        .expect("resolve absolute run target");
        assert_eq!(
            program,
            requested.canonicalize().expect("canonical program")
        );
        assert_eq!(
            cwd,
            requested
                .parent()
                .expect("program parent")
                .canonicalize()
                .expect("canonical program parent")
        );
    }

    #[test]
    fn runtime_inventory_covers_every_canonical_backend() {
        let registry_source = include_str!("../../../src/ir.rs");
        let registry_table = registry_source
            .split_once("const BACKEND_SPECS: &[BackendSpec] = &[")
            .expect("find canonical backend table")
            .1
            .split_once("\n];")
            .expect("find canonical backend table end")
            .0;
        let canonical = registry_table
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix('"')
                    .and_then(|tail| tail.split_once('"'))
                    .map(|(name, _)| name)
            })
            .collect::<BTreeSet<_>>();
        let discovered = BUILTIN_BACKENDS
            .iter()
            .copied()
            .chain(
                RUNTIME_SPECS
                    .iter()
                    .flat_map(|spec| spec.backends.iter().copied()),
            )
            .collect::<BTreeSet<_>>();

        assert_eq!(canonical.len(), 30, "canonical backend count changed");
        assert_eq!(discovered, canonical);
    }

    #[test]
    fn runtime_path_preserves_client_order_then_adds_local_fallbacks() {
        let fixture = Fixture::new();
        let home = fixture.0.join("home");
        let first = fixture.0.join("client-first");
        let second = fixture.0.join("client-second");
        let explicit = fixture.0.join("explicit-extra");
        for directory in [
            home.join(".local/bin"),
            fixture.0.join("target/release"),
            first.clone(),
            second.clone(),
            explicit.clone(),
        ] {
            fs::create_dir_all(directory).expect("create runtime path fixture");
        }
        let inherited = std::env::join_paths([&first, &second]).expect("join inherited PATH");
        let explicit_path = std::env::join_paths([&explicit]).expect("join explicit PATH");
        let encoded = runtime_search_path_from(
            &fixture.0,
            &home,
            Some(OsStr::new(&inherited)),
            Some(OsStr::new(&explicit_path)),
        );
        let paths = std::env::split_paths(&encoded).collect::<Vec<_>>();

        assert_eq!(&paths[..3], &[first, second, explicit]);
        assert!(paths.contains(&fixture.0.join("target/release")));
        assert!(paths.contains(&home.join(".local/bin")));
        assert_eq!(paths.iter().collect::<BTreeSet<_>>().len(), paths.len());
    }

    #[test]
    fn empty_tool_args_emit_strict_object_schema() {
        let schema = rmcp::handler::server::tool::schema_for_type::<EmptyArgs>();
        assert_eq!(schema.get("type"), Some(&serde_json::json!("object")));
        assert_eq!(schema.get("properties"), Some(&serde_json::json!({})));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_backend_descendants_before_they_can_commit() {
        let fixture = Fixture::new();
        let sentinel = fixture.0.join("late-backend-write");
        let command = format!("(sleep 2; printf late > '{}') & wait", sentinel.display());
        let error = run_cmd(
            PathBuf::from("/bin/sh").as_path(),
            &["-c", &command],
            None,
            &[],
            1,
        )
        .await
        .expect_err("backend process group must time out");
        assert_eq!(error, "timeout after 1s");
        tokio::time::sleep(std::time::Duration::from_millis(1_250)).await;
        assert!(
            !sentinel.exists(),
            "a backend descendant survived the timeout and wrote {}",
            sentinel.display()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_covers_pipe_drain_after_group_leader_exits() {
        let fixture = Fixture::new();
        let sentinel = fixture.0.join("late-write-after-leader-exit");
        let command = format!("(sleep 2; printf late > '{}') & exit 0", sentinel.display());
        let error = run_cmd(
            PathBuf::from("/bin/sh").as_path(),
            &["-c", &command],
            None,
            &[],
            1,
        )
        .await
        .expect_err("pipe drain and child wait must share one timeout");
        assert_eq!(error, "timeout after 1s");
        tokio::time::sleep(std::time::Duration::from_millis(1_250)).await;
        assert!(
            !sentinel.exists(),
            "a descendant retained the pipes and survived the timeout: {}",
            sentinel.display()
        );
    }
}
