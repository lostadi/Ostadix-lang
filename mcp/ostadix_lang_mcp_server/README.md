# ostadix-mcp (Rust-only)

Stdio MCP server for **Ostadix-lang / O-lang**. Agents call tools that always
resolve an **absolute** `O_BACKENDS_DIR`, so relative `backends` and bare
`$O_BACKENDS_DIR` splice mistakes do not break runs.

## Tools

| Tool | Purpose |
|------|---------|
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
only `o-info head --state ... --head ...`; no generic arguments, shell, cloud,
or network surface is exposed.

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
`o-lang` or write Information logical state. `o-info head` uses
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
crate does not link the root runtime and does not add a worker, scheduler lane,
or persistent `O` process.

Package 0.3 MCP execution remains deliberately local and uses fresh Graph V2
with `oexec.evidence/v6` and `oexec.admission/v6`; current CLI/API inspection
exposes Schedule Explanation/Why V2. Graph V1, Evidence/Admission V5, Schedule
Explanation/Why V1, and `PreparedPlacementFragmentV1` remain explicit archival
inspection surfaces only. The MCP never uplifts, relabels, authorizes, or
dispatches them as current V2/V6 authority. Execution Intent V1 stays bound to
the frozen Graph V1 identity, but a matching handle carries no authority and
forces fresh Graph V2/V6 admission before dispatch.

Hosted Placement V6 is a separate milestone. Its current preparation boundary
is `PreparedPlacementFragmentV2`; the authenticated direct-node surface is the
`octl node ...` client and `o-node` service documented in
[`docs/HOSTED_PLACEMENT_V6.md`](../../docs/HOSTED_PLACEMENT_V6.md). This MCP
does not discover a federated registry, enroll a node, request a placement
lease, or turn a stable intent handle into placement authority. No MCP tool
wraps frozen one-operation V1, durable session V2, placement-authority issuance
or the co-located development mint, explicit closed-session GC, or the separate
local `o-registry` snapshot store. In particular, no MCP tool holds a session
bearer, submits `PlacementLeaseV2`, consumes a V2 signed journal receipt, or
opens or upgrades a durable state root; `o-node` rejects durable state without
the exact package-0.3 execution-authority marker, while a fresh empty root may
be initialized with that marker.

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

1. Prefer `o_analyze_intent` + `o_execute_intent` when the action must remain
   bound to inspected source and graph intent; `o_run` is direct execution.
2. Never pass the literal string `O_BACKENDS_DIR` as the backends argv.
3. Never put `$VAR` / `$O_BACKENDS_DIR` **inside** `.O` sources (O splices `$IDENT`).
4. Always use an absolute backends directory.

## Stack

- Rust 2021
- [rmcp](https://crates.io/crates/rmcp) 0.6.x (`server` + `transport-io`)
- Logging: **stderr** only (stdout is JSON-RPC)
