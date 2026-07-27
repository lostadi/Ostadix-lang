# ostadix-mcp (Rust-only)

Stdio MCP server for **Ostadix-lang / O-lang**. Agents call tools that always
resolve an **absolute** `O_BACKENDS_DIR`, so relative `backends` and bare
`$O_BACKENDS_DIR` splice mistakes do not break runs.

## Tools

| Tool | Purpose |
|------|---------|
| `o_env` | Print `O_LANG_ROOT`, backends, `O` / `olangc` paths, shim presence |
| `o_doctor` | Existence checks + shim inventory + a18re `search/o-run` |
| `o_smoke` | `O examples/hello.O <absolute-backends>` — expect `2` |
| `o_run` | Run any `.O` with absolute backends |
| `o_olangc` | `olangc` with `--shim-dir` (targets: ir, dot, script, wasm, …) |
| `o_search_run` | Run `~/a18re/search/<name>.O` with correct env |

## Build / install

```bash
cd ~/Ostadix-lang/mcp/ostadix_lang_mcp_server
cargo build --release
cp -f target/release/ostadix-mcp ~/.local/bin/ostadix-mcp
```

## Grok config (`~/.grok/config.toml`)

```toml
[mcp_servers.olang]
command = "/Users/ustad/.local/bin/ostadix-mcp"
args = []
env = {
  O_LANG_ROOT = "/Users/ustad/Ostadix-lang",
  O_BACKENDS_DIR = "/Users/ustad/Ostadix-lang/backends",
  PATH = "/Users/ustad/.local/bin:/Users/ustad/Ostadix-lang/target/release:/usr/bin:/bin"
}
enabled = true
```

Reload MCP / restart the session so tools appear as `olang__o_run`, etc.

## Agent rules (encoded in tool instructions)

1. Prefer `o_run` / `o_search_run` over raw shell `O … backends`.
2. Never pass the literal string `O_BACKENDS_DIR` as the backends argv.
3. Never put `$VAR` / `$O_BACKENDS_DIR` **inside** `.O` sources (O splices `$IDENT`).
4. Always use an absolute backends directory.

## Stack

- Rust 2021
- [rmcp](https://crates.io/crates/rmcp) 0.6.x (`server` + `transport-io`)
- Logging: **stderr** only (stdout is JSON-RPC)
