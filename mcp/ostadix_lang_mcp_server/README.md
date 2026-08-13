# ostadix-mcp (Rust-only)

Stdio MCP server for **Ostadix-lang / O-lang**. Agents call tools that always
resolve an **absolute** `O_BACKENDS_DIR`, so relative `backends` and bare
`$O_BACKENDS_DIR` splice mistakes do not break runs.

## Tools

| Tool | Purpose |
|------|---------|
| `o_env` | Print roots, `O` / `olangc` paths, shim presence, and the 30-backend runtime summary |
| `o_runtimes` | Report executable discovery for every canonical backend and each supported alternative runtime set |
| `o_doctor` | Existence checks + shim inventory + complete runtime report + a18re `search/o-run` |
| `o_smoke` | `O examples/hello.O <absolute-backends>` — expect `2` |
| `o_analyze_intent` | Nonexecutingly compute a stable execution intent and return a bounded, expiring, one-use opaque handle |
| `o_execute_intent` | Consume that handle and require `O` to recompute the same source and execution-intent digests before fresh V5 admission and dispatch |
| `o_run` | Direct, ungated compatibility execution of any `.O` with absolute backends; relative input resolves once against `cwd` or the repository root, while an absolute path with no `cwd` runs from its parent directory |
| `o_olangc` | `olangc` with `--shim-dir`; relative input/output resolves against the repository root |
| `o_search_run` | Run `~/a18re/search/<name>.O` with correct env |

## Build / install

```bash
cd ~/Ostadix-lang/mcp/ostadix_lang_mcp_server
cargo build --release --locked
cp -f target/release/ostadix-mcp ~/.local/bin/ostadix-mcp
```

From the repository root, run the supported release checks with:

```bash
cargo test --locked --manifest-path mcp/ostadix_lang_mcp_server/Cargo.toml
cargo clippy --locked --manifest-path mcp/ostadix_lang_mcp_server/Cargo.toml -- -D warnings
cargo build --release --locked --manifest-path mcp/ostadix_lang_mcp_server/Cargo.toml
python3 scripts/smoke_ostadix_mcp.py
```

The last command performs a real MCP initialize/list/call exchange and requires
the root release `O` and `olangc` binaries. Under a deliberately system-only
`PATH`, it validates every tool's object schema, calls `o_runtimes`, `o_smoke`,
both supported relative-path forms of `o_run`, and relative-path `o_olangc`.
The client drains stdout/stderr concurrently and retains out-of-order JSON-RPC
replies by id.

## Same-intent execution gate

For an inspect-then-execute flow, call:

```text
o_analyze_intent {"path":"program.O","cwd":"project"}
o_execute_intent {"handle":"<opaque>","path":"program.O","cwd":"project"}
```

`o_analyze_intent` asks released `olangc` for
`oexec.execution-intent/v1` without executing the program. The MCP process
keeps at most 64 live or in-progress records, reserving a slot before spawning
`olangc` so rejected overflow cannot consume analysis capacity. A handle expires
after 120 seconds by default, may request 1 through 900 seconds, and is consumed before target validation or
execution. Reuse, expiration, a different canonical program/cwd/root/backends,
or a changed source fails closed. `o_execute_intent` supplies the analyzed
source and stable-intent digests to `O`; `O` recomputes them and then constructs
a fresh V4 `AdmittedExecution`, which remains the sole dispatch authority.

This protocol is a local **same-intent gate**, not authorization, a capability,
a retained admission object, proof of runtime health, or a capacity lease.
`o_run` remains available as an explicitly ungated compatibility path. The MCP
crate does not link the root runtime and does not add a worker, scheduler lane,
or persistent `O` process.

The checked-in `.mcp.json` contains no shell expressions. When explicit
environment paths are absent, the server recognizes the repository from its
working directory or an ancestor by checking the root Cargo package, Python
shim, and hello example. It does not contain a developer-specific absolute
fallback. The crate is distributed under `LGPL-2.1-only`, matching the root
license shipped in the source release.

## Runtime discovery

At startup, the server preserves the client's `PATH` order and appends existing
local runtime locations commonly omitted by GUI/MCP launchers: repository and
user bins, Homebrew, Nix profiles, mise/asdf/pyenv/rbenv, Conda, Volta/fnm,
GHCup/OPAM, .NET, Wasmtime/Wasmer, and SDKMAN Java. Set
`OSTADIX_RUNTIME_PATH` to append additional explicit directories.

`OSTADIX_RUNTIME_PATH_MODE` selects the search policy:

- `discover-local` (default) preserves inherited entries, appends explicit
  entries, and then adds existing repository, user, runtime-manager, and system
  fallbacks;
- `inherited-plus-explicit` uses only inherited entries followed by
  `OSTADIX_RUNTIME_PATH`;
- `inherited-only` ignores explicit and discovered additions.

Unknown mode values fail startup. The ordered path and its provenance are
captured once before the process `PATH` changes; `o_runtimes` reports that same
immutable view using `runtime-search-entry` and `path-sources` records. Thus a
fallback selected by discovery cannot later be mislabeled as client-inherited.

`o_runtimes` projects every canonical backend and its ordered executable
alternatives directly from `src/backend_catalog.inc.rs`; it has no independent
source inventory to synchronize. The report labels this as a compiled MCP
snapshot: rebuild `ostadix-mcp` whenever the root catalog changes, since the
dependency-isolated installed server and `O` may otherwise be different build
generations. Builtin backends are identified separately;
external backends report the first complete executable alternative found, or
every acceptable alternative when missing. This is a non-executing presence
check, not a permission grant or a runtime health claim. The backend adapter
still validates and launches the selected tools. Each runtime line reports
`precision=exact` or `precision=conservative-all-sources`; the latter marks a
safe catalog-wide over-approximation that operation-specific analysis may
later refine. The output makes the evidence ladder explicit: this tool
establishes only `declared` and `located`; `invocable`, `compatible`,
`authorized`, `healthy`, and per-operation `admitted` remain not-probed or
deferred to their actual operation-scoped mechanisms.

## Grok config (`~/.grok/config.toml`)

```toml
[mcp_servers.olang]
command = "/Users/ustad/.local/bin/ostadix-mcp"
args = []
env = {
  O_LANG_ROOT = "/Users/ustad/Ostadix-lang",
  O_BACKENDS_DIR = "/Users/ustad/Ostadix-lang/backends"
}
enabled = true
```

Reload MCP / restart the session so tools appear as `olang__o_runtimes`,
`olang__o_run`, etc.

## Agent rules (encoded in tool instructions)

1. Prefer `o_analyze_intent` + `o_execute_intent` when the action must remain
   bound to inspected source and graph intent; `o_run` is direct execution.
2. Never pass the literal string `O_BACKENDS_DIR` as the backends argv.
3. Never put `$VAR` / `$O_BACKENDS_DIR` **inside** `.O` sources (O splices `$IDENT`).
4. Always use an absolute backends directory.

## Stack

- Rust 2021
- [rmcp](https://crates.io/crates/rmcp) 0.6.x (`server` + `transport-io`)
- Logging: **stderr** only (stdout is JSON-RPC)
