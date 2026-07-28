# Ostadix-lang — agent instructions

This is the **canonical** O / Ostadix-lang runtime monorepo.

## Roots

```bash
export O_LANG_ROOT=/Users/ustad/Ostadix-lang
export O_BACKENDS_DIR=$O_LANG_ROOT/backends
export PATH="$O_LANG_ROOT/target/release:$HOME/.local/bin:$PATH"
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

`mcp/ostadix_lang_mcp_server` — Rust/`rmcp` stdio MCP server exposing `o_env`,
`o_doctor`, `o_smoke`, `o_run`, `o_olangc`, `o_search_run` so agents don't
rediscover the relative-`backends` / `$VAR`-splice traps by hand. Own
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
