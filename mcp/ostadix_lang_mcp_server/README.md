# ostadix-mcp (Rust-only)

Stdio MCP server for **Ostadix-lang / O-lang**. Agents can discover the
installed toolchain, read task guides, invoke its command families with literal
arguments, evaluate inline `.O`, and manage concurrent or interactive jobs.
The server supplies an **absolute** `O_BACKENDS_DIR`, so relative `backends`
and bare `$O_BACKENDS_DIR` splice mistakes do not break runs.

## Tools

| Tool | Purpose |
|------|---------|
| `o_capabilities` | Search the command catalog, resolved executable availability, documentation paths, and related guide topics; optional `query` filters the inventory |
| `o_guide` | Read a task guide; `topic` defaults to the overview, with runtime/compiler/projects/mesh/core/live/capacity/device/agents guidance |
| `o_cli` | Invoke a catalog command with its complete literal `args` array, optional cwd/env/stdin, timeout, background execution, and PTY |
| `o_eval` | Evaluate inline `.O` through the installed interpreter, with per-call cwd/env/interpreter arguments and optional background execution or PTY |
| `o_job_list` | List the jobs owned by this MCP session without waiting for running work |
| `o_job_status` | Inspect one session job's state, exit status, log sizes, input state, and process cleanup evidence |
| `o_job_read` | Read stdout or stderr by byte offset with a bounded page and continuation cursor |
| `o_job_write` | Send input to a running job and optionally close its input |
| `o_job_cancel` | Cancel one job and clean up its Unix process session, including nested process groups |
| `o_env` | Print roots, `O` / `olangc` paths, shim presence, and the 30-backend runtime summary |
| `o_runtimes` | Report executable discovery and catalog value capabilities for every canonical backend and supported alternative runtime set |
| `o_doctor` | Existence checks + shim inventory + complete runtime report + resolved external or bundled search corpus |
| `o_smoke` | `O examples/hello.O <absolute-backends>` — expect `2` |
| `o_analyze_intent` | Nonexecutingly compute a stable execution intent and return a bounded, expiring, one-use opaque handle |
| `o_execute_intent` | Consume that handle and require `O` to recompute the same source and execution-intent digests before fresh Graph V2/Evidence and Admission V6 dispatch |
| `o_run` | Direct, ungated compatibility execution of any `.O` with absolute backends; relative input resolves once against `cwd` or the repository root, while an absolute path with no `cwd` runs from its parent directory |
| `o_olangc` | `olangc` with `--shim-dir`; relative input/output resolves against the repository root. `materialize_only` admits ordinary binary/WASM inputs, requires a new contained destination below the server cwd, rejects traversal/existing targets, and invokes neither Cargo nor output publication. |
| `o_search_run` | Run one strict leaf name from `<work>/search`, or bundled `examples/` when no external work tree exists; reject traversal and symlink escape |
| `o_information_inspect` | Fixed, bounded `o-info head` inspection of one existing local Information V1 root; returns sanitized IDs/count, no state path or authority, and makes no logical/content/inode/mode/mtime change (atime untested) |

## Agent workflow

Start with `o_capabilities` and `o_guide`, then use `o_env`, `o_runtimes`, and
`o_doctor` for the selected installation. The catalog covers the shipped Cargo
binaries and supported dispatch/script entry points, including interpreter,
compiler, project linking, O-core, mesh/node operations, Live-World, O-Git,
kernel/capacity tooling, notebook, language server, and build/release helpers.
Filter it by task or command name to keep discovery output focused.
MCP resource clients can read the same catalog at `ostadix://capabilities`
and guides at `ostadix://guide/{topic}`; `resources/list` enumerates them.

Catalog `available` means the executable or script interpreter was located.
It does not establish that every dependency, backend, target, node, device, or
requested operation works. `runtime_readiness_verified` and
`installed_help_verified` make that boundary explicit. Use the reported
`help_args` where present to inspect the selected executable's own interface;
some service entry points do not support a conventional help invocation.
The catalog is compiled from the source version shipped with this MCP; it can
differ from separately installed CLI binaries until they are rebuilt together.

The direct command gateway preserves all CLI options. For example:

```json
{"command":"olangc","args":["/project/main.O","--target","dot","--shim-dir","/absolute/backends"],"cwd":"/project"}
```

Pass a catalog command identifier such as `O`, `o`, `olangc`, `o-link`, `octl`,
or `ocorec` as `command`. The gateway resolves its executable and supplies the
argument vector without shell interpolation. `args` contains individual
arguments; shell operators, quoting, glob expansion, and variable substitution
are not a command language. Use `env` for per-process environment values and
`cwd` for the working directory. Environment changes do not modify the MCP
server's environment or other jobs. All CLI flags remain subject to the
selected tool's actual parsing and runtime admission rules.

Inline programs avoid temporary-source bookkeeping:

```json
{"source":"python^(\n__oval_result__ = 1 + 1\n)_python","cwd":"/project"}
```

`o_eval` still executes ordinary O syntax and its selected backend. `$IDENT`
inside an O source is a splice, including when the source is a JSON string;
pass environment values through `env`, and let the hosted language read them.
Use the original `o_analyze_intent` / `o_execute_intent` pair when execution
must be bound to the analyzed source and graph intent.

## Long-running and interactive jobs

Set `background: true` on `o_cli` or `o_eval` for a managed job that returns
immediately. Jobs are independent: waiting for one does not serialize another
job's execution. They survive individual tool calls within the same MCP
session; they do not become detached persistent services after MCP shutdown.
Use the native service-management mechanism when persistence across agent
sessions is required.

For example, an O REPL can be started with:

```json
{"command":"O","args":["--repl","/absolute/backends"],"background":true,"pty":true}
```

Use its returned `job_id` with `o_job_write` to send a line, `o_job_read` to
inspect output, and `o_job_status` to inspect progress. `close: true` closes
pipe input. PTY input closure sends terminal EOF characters; programs using
raw terminal input may interpret those characters themselves. PTYs combine
stdout and stderr into the stdout log. Input writes have a 30-second default
timeout covering both input-lock waiting and transmission; `timeout_secs: 0`
explicitly permits an unbounded write. A write timeout reports the accepted
byte count and whether EOF delivery is uncertain.

Managed Unix jobs start in a fresh operating-system process session.
`o_job_cancel` stops that session's processes, including backend-owned nested
process groups; a subprocess that explicitly creates another session is
outside this boundary. Cleanup evidence reports the scan, signaling, child
reaping, and completed log drainage. Normal successful completion preserves
the native command's daemon-start behavior.

Foreground calls return a job result with bounded stdout/stderr previews and
retained log locations. Nonzero exit, timeout, and cancellation are MCP tool
errors with job/exit evidence. New tools provide the same JSON object as both
`structuredContent` and text so clients supporting either representation can
read it. The original ten tools retain their text contracts.

`o_job_read` accepts `stream: "stdout" | "stderr"`, `offset`, and `limit`.
Advance using `next_offset`; offsets and `bytes_read` count bytes, while `text`
uses UTF-8 replacement decoding by default. Select `encoding: "base64"` for
lossless bytes in the `data` field, including split multibyte characters.
The full log artifact is also available. `eof` means the current end of
the log; check job state or `complete` to distinguish a running job from a
finished one. Logs are written to disk to avoid retaining unlimited process
output in the server heap.

## Agents with short-lived function wrappers

Agents that start a new function process for every tool call can use
[`scripts/ostadix_mcp_client.py`](../../scripts/ostadix_mcp_client.py). It keeps
one stdio MCP session behind a private local Unix socket so a background job
created by one invocation remains available to later invocations:

```bash
python3 scripts/ostadix_mcp_client.py --list-tools
python3 scripts/ostadix_mcp_client.py o_eval '{"source":"python^( __oval_result__ = 2 )_python","background":true}'
python3 scripts/ostadix_mcp_client.py o_job_list '{}'
python3 scripts/ostadix_mcp_client.py --status
python3 scripts/ostadix_mcp_client.py --stop
```

The bridge uses `OSTADIX_MCP` for the server executable, `O_LANG_ROOT` and
`O_BACKENDS_DIR` for the installation, and optional `OSTADIX_MCP_CLIENT_DIR`
for its private directory. The default directory is
`/tmp/ostadix-mcp-client-<uid>`, mode 0700, with mode 0600 lock and log files.
Binary path, root, and backends determine session identity. Startup is locked;
concurrent calls share one MCP child and route replies by JSON-RPC request ID.
Child stderr goes directly to a log, and a nonblocking writer keeps stalled
MCP stdin from blocking unrelated bridge requests or shutdown.

`--list-tools` exports the MCP's actual tool schemas for function-registration
generation. `--status` inspects an existing bridge without starting one.
`--stop` closes the MCP transport and waits for that bridge's processes to
exit, including the MCP's managed-job cleanup. Restart the bridge explicitly
after replacing the MCP binary or changing inherited runtime configuration;
the bridge does not discard active jobs automatically.

A disconnected or timed-out foreground caller sends MCP cancellation for an
already-transmitted request. A request still queued for transmission is
discarded. If transmission started, the error marks execution as uncertain;
the bridge never retries such a request. Explicit background calls survive
their caller's exit and remain discoverable through `o_job_list`.
Structured tool output and `isError` are preserved, while successful legacy
text tools retain their existing text output. The default transport wait is
600 seconds (`OSTADIX_MCP_CLIENT_TIMEOUT` can change it), extended to at least
30 seconds beyond a positive tool `timeout_secs`. An explicit client
`--timeout` overrides this calculation. Tool `timeout_secs: 0` and client
`--timeout 0` both preserve an unbounded foreground wait; client disconnection
still cancels a pending foreground request. Background jobs allow such
operations to continue without keeping a function invocation attached.
The bridge has a 16 MiB JSON
frame bound; use job-log pagination for large output. Its socket is local to
the user account, with no network listener or cross-machine protocol.

## Build / install

Development builds on Lee's machine belong in a native path inside the
`moral-gaur` Multipass VM, not in the mounted macOS checkout. With an exact
source snapshot at `$OSTADIX_GUEST_SOURCE` inside that VM:

```bash
cd "$OSTADIX_GUEST_SOURCE"
cargo build --release --locked --package o-lang --bin O --bin olangc --bin o-info
cd "$OSTADIX_GUEST_SOURCE/mcp/ostadix_lang_mcp_server"
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
the root release `O`, `olangc`, and `o-info` binaries. Under a deliberately system-only
`PATH`, it validates every tool's object schema, calls `o_runtimes`, `o_smoke`,
both supported relative-path forms of `o_run`, relative-path `o_olangc`, and
bundled `o_search_run`, rejects search-path escape, and performs fixed local
Information V1 head inspection with a no-mutation tree comparison.
It also checks the new structured and text response contracts, command
discovery and guide resources, literal per-job environment/cwd handling,
inline evaluation, CLI failures, independent background jobs, byte-paged
logs, REPL stdin/EOF, and cancellation of a delayed descendant. All execution
fixtures use disposable local state; this smoke does not exercise every
cataloged command, start a real node, install software, or contact a remote
service. PTY behavior has separate execution-layer tests.
The client drains stdout/stderr concurrently and retains out-of-order JSON-RPC
replies by id.

## Read-only Information inspection

`o_information_inspect` accepts only an existing, non-symlink Information V1
state root, one bounded head token, and a timeout. It resolves the fixed
repository `target/release/o-info` binary by default. An installed image may set
`OSTADIX_O_INFO_BIN` to an explicit absolute path such as
`/usr/local/bin/o-info`; relative, missing, non-executable, and final-component
symlink paths are rejected. The dedicated runner clears the inherited environment, captures
stdout and stderr concurrently through hard byte limits, kills and reaps the
process group on Unix (the direct child elsewhere) on overflow or timeout,
rejects non-UTF-8/control/unexpected or duplicate output, and never returns raw
stderr or the state path. It invokes
only `o-info head --state ... --head ...`. This fixed inspector has no generic
arguments. The separate `o_cli` tool exposes the full `o-info` CLI and its
possible effects; it does not inherit this inspector's read-only guarantee.

The installed-layout transport smoke is:

```bash
python3 scripts/smoke_ostadix_mcp.py \
  --root /usr/src/ostadix \
  --binary /usr/local/bin/ostadix-mcp \
  --server-cwd /workspace \
  --require-wasm-materialization \
  --wasm-release-manifest /usr/share/ostadix/wasm/hello.release.json \
  --wasm-release-artifact /usr/share/ostadix/wasm/hello.wasm \
  --wasm-source-tree "$STAGED_TREE" \
  --wasm-base-commit "$BASE_COMMIT" \
  --wasm-source-archive-sha256 "$SOURCE_ARCHIVE_SHA256" \
  --o-info /usr/local/bin/o-info \
  --runtime-bin-dir /usr/local/bin
```

The smoke child pins `PYTHONDONTWRITEBYTECODE=1`, preserving the exact staged
source tree while the Python backend is exercised. Materialization uses
`examples/wasm_hello.O` and proves the MCP-exposed compiler can regenerate the
descriptor-bound Cargo project without invoking Cargo or producing the output
artifact. It is not runtime-execution evidence. Stage-two boot separately runs
the admitted module and `examples/webassembly_hello.O` under Wasmtime;
`--require-wasm` remains available as a slower focused cold-compilation test
outside the normal boot path.

The root runtime remains an independent child: the MCP crate does not link
`o-lang`. The fixed inspector does not write Information logical state;
`o-info head` uses
`InformationStoreReaderV1`, which creates no directory/lock, repairs no mode,
and updates no head. The sanitized result is descriptive metadata only.
Information presence, a verified pack, World `signature_validated`, and Hosted
self-signature consistency grant no execution authority, freshness, signer
trust, or journal continuity.

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
a fresh Graph V2/V6 `AdmittedExecution`, which remains the sole dispatch
authority.

This protocol is a local **same-intent gate**, not authorization, a capability,
a retained admission object, proof of runtime health, or a capacity lease.
`o_run` remains available as an explicitly ungated compatibility path. The MCP
crate does not link the root runtime or change its worker and scheduler
configuration. Managed background jobs may keep an `O` process alive for the
duration of the MCP session.

The existing local execution tools retain fresh Graph V2 with
`oexec.evidence/v6` and `oexec.admission/v6`; current CLI/API inspection
exposes Schedule Explanation/Why V2. Graph V1, Evidence/Admission V5, Schedule
Explanation/Why V1, and `PreparedPlacementFragmentV1` remain explicit archival
inspection surfaces only. The MCP never uplifts, relabels, authorizes, or
dispatches them as current V2/V6 authority. Execution Intent V1 stays bound to
the frozen Graph V1 identity, but a matching handle carries no authority and
forces fresh Graph V2/V6 admission before dispatch.

Hosted Placement V6 uses `PreparedPlacementFragmentV2`; its authenticated
direct-node surface is the `octl node ...` client and `o-node` service documented
in [`docs/HOSTED_PLACEMENT_V6.md`](../../docs/HOSTED_PLACEMENT_V6.md).
`o_cli` exposes their complete arguments along with registry and related
command families. It supplies process execution, input, logs, and lifecycle
control; the native CLI retains responsibility for credentials, admission,
placement leases, durable sessions, receipt verification, and state-version
checks. Neither a catalog entry nor a same-intent handle supplies missing
authority or upgrades an old execution identity. Inspect the mesh guide and
the selected CLI's help for the current operation before invoking it.

The checked-in `.mcp.json` contains no shell expressions. When explicit
environment paths are absent, the server recognizes the repository from its
working directory or an ancestor by checking the root Cargo package, Python
shim, and hello example. Installed media also recognizes the validated
`/usr/src/ostadix` source root, so launching from `/workspace` does not depend
on current-directory accident. It does not contain a developer-specific
fallback. The crate is distributed under `LGPL-2.1-only`, matching the root
license shipped in the source release.

## Runtime discovery

At startup, the server preserves the client's `PATH` order and appends existing
local runtime locations commonly omitted by GUI/MCP launchers: repository and
user bins, Homebrew, Nix profiles, mise/asdf/pyenv/rbenv, Conda, Volta/fnm,
GHCup/OPAM, .NET, Wasmtime/Wasmer, SDKMAN Java, and the Termux package prefix
(from its environment or the package-independent `.../files/home` layout). Set
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
alternatives directly from `crates/ostadix-api/src/backend_catalog.inc.rs`; it has no independent
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

The current compiled catalog schema is `ostadix.backend-catalog/v6`.
`o_runtimes` exposes it as
`runtime-catalog-schema=ostadix.backend-catalog/v6`. The schema participates in
both the complete catalog digest and every backend-specification digest, so a
V5 MCP binary is an older descriptive snapshot rather than a source of V6
placement identity. Rebuild the root runtime and this dependency-isolated MCP
crate together after a catalog change (the root `./setup.sh --minimal --yes`
flow does so), then restart MCP clients. Never relabel a digest reported by an
old binary. Archived V5, V4, and V3 records may still be decoded and their original
signatures inspected, but that is not placement authorization; current
`NodeProfileV1` validation accepts only backend specifications present in the
V6 registry. V4 remains frozen with each backend's state-support tier and
snapshot-compatibility identity. V5 extends that exact projection with an
explicit optional bounded backend-morphism profile. V6 retains those fields and
adds the two `wasm-tools` WebAssembly runtime alternatives after the frozen WABT pair.

The same dependency-isolated catalog macro emits one `runtime-capability`
record per backend with `integer-exactness`, `rich-numbers`, `state-support`,
`morphism-profile`, and the applicable state-codec/compatibility or
external-manifest fields. A profile value names a bounded shadow crossing; it
does not authorize execution or claim a generic backend crossing. These are
typed catalog declarations used by conservative fidelity and placement
analysis, not runtime probes, proof that a checkpoint currently succeeds, or
placement warrants.
Unknown capability remains explicit and cannot be promoted to a lossless
crossing merely because two aliases share a language name.

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

1. Discover the current command families with `o_capabilities`, read the
   relevant `o_guide`, and check the selected runtime with the environment tools.
2. Use `o_cli` for complete CLI options, `o_eval` for inline programs, and job
   tools for concurrent, long-running, or interactive work.
3. Prefer `o_analyze_intent` + `o_execute_intent` when the action must remain
   bound to inspected source and graph intent; `o_run` is direct execution.
4. Never pass the literal string `O_BACKENDS_DIR` as the backends argv.
5. Never put `$VAR` / `$O_BACKENDS_DIR` **inside** `.O` sources (O splices `$IDENT`).
6. Always use an absolute backends directory.

## Stack

- Rust 2021
- [rmcp](https://crates.io/crates/rmcp) 0.6.x (`server` + `transport-io`)
- Logging: **stderr** only (stdout is JSON-RPC)
