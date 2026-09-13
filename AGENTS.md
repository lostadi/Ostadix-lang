# Ostadix-lang — agent instructions

This is the **canonical** O / Ostadix-lang runtime monorepo.

## Roots

```bash
export O_LANG_ROOT=/Users/ustad/Ostadix-lang
export O_BACKENDS_DIR=$O_LANG_ROOT/backends
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$O_LANG_ROOT/target/release:$PATH"
```

Do **not** use `~/O-lang` for builds/runs on this machine.

## Toolchain

| Goal | Command |
|------|---------|
| Run `.O` | `O file.O backends` or `o run file.O` |
| IR / plan | `olangc file.O --target ir --shim-dir backends` or `o plan file.O` |
| AOT | `olangc file.O -o out --shim-dir backends` or `o ship file.O` |
| Link | `o-link paths -o app.O` |
| Live-World | `o-live-host demo --state DIR` or `o live demo` |
| O-Git | `ogit demo semantic-receipt` or `o receipt` |
| O-core | `ocorec file.oc --emit mir` |

## MCP server

`mcp/ostadix_lang_mcp_server` — Rust/`rmcp` stdio MCP server. Start with
`o_capabilities` (optional query) and `o_guide` (workflow topic) to discover the
full Ostadix command surface. `o_cli` accepts exact native argument arrays,
cwd, per-child environment, stdin, optional Unix PTY, and background execution.
`o_eval` runs inline polyglot O source. Long or interactive work uses
`o_job_list`, `o_job_status`, `o_job_read`, `o_job_write`, and `o_job_cancel`;
jobs run independently and retain full output on disk with bounded pages.
`o_job_read` can return lossless base64 bytes. Jobs and intent handles belong to
one MCP session, so short-lived CLI clients should use the persistent local
`scripts/ostadix_mcp_client.py` bridge.

Existing `o_env`, `o_runtimes`, `o_doctor`, `o_smoke`, `o_run`, `o_olangc`,
`o_search_run`, `o_analyze_intent`, `o_execute_intent`, and fixed read-only
`o_information_inspect` remain available. Catalog presence is not installed
version, health, or admission proof; use the listed supported help invocation
to inspect the selected CLI. Complete CLI access retains each command's native
admission rules and does not turn the read-only Information inspector into a
write API. Resources also expose the catalog and ten workflow guides. Own
`Cargo.lock` (not a workspace member) so `rmcp`/`tokio full` stay out of the
main O-lang build.

Built by `setup.sh` (`build_mcp_server`, skip with `--no-mcp`) via
`cargo build --release --locked`; installs the `ostadix-mcp` wrapper into
`~/.local/bin`. Registered for MCP clients (Claude Code included) via
`.mcp.json` at repo root. Rebuild directly with:

```bash
cargo build --release --locked --manifest-path mcp/ostadix_lang_mcp_server/Cargo.toml
```

## Skills

Load via skill tool when relevant: `olang`, `olang-runtime`, `olang-ocore`, `ostadix-control`, `ostadix-wasm`, `ostadix-term`.

## Terminal kit

`~/.config/ostadix/term/ostadix-term.zsh` — `o doctor`, `o plan`, `o live`, `o receipt`.

## Evidence

Show real command output. Smoke: `O examples/hello.O backends` → `2`.
