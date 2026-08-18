## User

do not modify anything just tell me about this repo

## Assistant

I'll stay read-only and survey the repo from its own docs and layout, then tell you what it actually is.

## Tools

- Read: /Users/ustad/.grok/skills/olang-runtime/SKILL.md
- Read: /Users/ustad/Ostadix-lang/README.md (1-200)
- ListDir: .
- IntegrationSearch (MCP tool discovery)

## Assistant

I have the top-level map. Next I’ll read the claims, architecture, and crate layout so the description matches what this tree actually contains.

## Tools

- UseTool: olang__o_env
- Read: /Users/ustad/Ostadix-lang/docs/CLAIMS.md (1-150)
- Read: /Users/ustad/Ostadix-lang/ARCHITECTURE.md (1-120)
- Read: /Users/ustad/Ostadix-lang/Cargo.toml
- Read: /Users/ustad/Ostadix-lang/ORIGIN.md (1-24)
- Read: /Users/ustad/Ostadix-lang/CITATION.cff
- Read: /Users/ustad/Ostadix-lang/SPEC.md (1-80)
- Read: /Users/ustad/Ostadix-lang/README.md (201-400)
- Search: registered_backends|canonical_tag|BackendSpec
- ListDir: docs
- ListDir: src
- ListDir: backends
- Read: /Users/ustad/Ostadix-lang/LICENSE (1-20)
- Read: /Users/ustad/Ostadix-lang/CHANGELOG.md (1-40)
- Read: /Users/ustad/Ostadix-lang/src/backend_catalog.inc.rs (1-80)
- Search: ^\| `
- Read: /Users/ustad/Ostadix-lang/examples/hello.O
- Read: /Users/ustad/Ostadix-lang/ocore/README.md (1-60)
- Read: /Users/ustad/Ostadix-lang/docs/VERSIONING.md (1-80)
- Execute: git -C /Users/ustad/Ostadix-lang log -8 --oneline && echo '---' && git -C /Users/ustad/Ostadix-lang rev-parse --short HEAD && git -C /Users/ustad/Ostadix-lang status -sb && echo '---' && git -C /Users/ustad/Ostadix-lang remote -v && echo '---' && ls -la /Users/ustad/Ostadix-lang/Ostadix-lang 2>/dev/null | head -5; file /Users/ustad/Ostadix-lang/Ostadix-lang 2>/dev/null; ls /Users/ustad/Ostadix-lang | wc -l (Read git identity and nested-tree clues)
- Search: backend_spec!
- Read: /Users/ustad/Ostadix-lang/README.md (2681-2760)
- Execute: python3 -c "
p='/Users/ustad/Ostadix-lang/examples/hello.O'
import pathlib
b=pathlib.Path(p).read_bytes()
print('bytes', len(b))
print('repr', b[:200])
print('text', b.decode('utf-8', errors='replace'))
" && echo '--- nested ---' && ls /Users/ustad/Ostadix-lang/Ostadix-lang | head && echo '--- crates ---' && ls /Users/ustad/Ostadix-lang/mcp && echo '--- tests ---' && ls /Users/ustad/Ostadix-lang/tests | head -40 && echo '--- rust-toolchain ---' && cat /Users/ustad/Ostadix-lang/rust-toolchain.toml (Decode hello.O and inspect nested tree)
- Read: /Users/ustad/Ostadix-lang/docs/CLAIMS.md (2-16)
- Read: /Users/ustad/Ostadix-lang/README.md (2-36)
- Search: ^backend_
- Read: /Users/ustad/Ostadix-lang/src/backend_catalog.inc.rs (201-280)
- Read: /Users/ustad/Ostadix-lang/AGENTS.md (1-40)

## Assistant

Lee, this tree is **your** Ostadix-lang / O-lang runtime monorepo — the compatibility-preserving integration repo for **OSTADIX**. I did not change anything.

Canonical identity, as the tree itself records it:

- **Crate:** `o-lang` **0.2.0**, Rust **1.93.1** MSRV, pin **1.97.1**
- **License:** LGPL-2.1-only
- **Author:** Lee Daghlar Ostadi
- **Remote:** `https://github.com/lostadi/Ostadix-lang`
- **HEAD I looked at:** `45256b5a` on `master`, matching `origin/master`
- **Paper phrase:** *The nesting is the interface* (Zenodo `10.5281/zenodo.21544345`)

It is not a Python wrapper, a notebook host, or “polyglot scripting.” The claim is a **conjunction**: expression-granular evaluator selection + recursive nestability + a compile-time registry of evaluators + independently implemented real runtimes + one value domain + whole-program lowering.

---

## What the system is

OSTADIX is the umbrella. This repo keeps the component names distinct:

| Name | Job in this tree |
|---|---|
| **Ostadix-lang** | Hosted polyglot language and evidence-bound HGraph runtime (`.O`) |
| **O-core** | Freestanding native systems language (`.oc` → HIR → SSA MIR → ELF) |
| **OKernel** | Sovereign kernel built *through* O-core (`ocore/kernel/`) |
| **O-Machine** | Architecture-specific machine-resource / virtualization substrate (research, bounded records) |
| **World** | Distributed runtime ontology — identities, resources, namespaces, contracts — not the release name |

The first integrated system release is named **OSTADIX Alpha**. `World` is a runtime concept, not a branding suffix.

The hosted thesis is written into the syntax itself:

```O
html^(
  <p>The answer is python^(
__oval_result__ = sum(x*x for x in range(10))
)_python.</p>
)_html
```

`LANG^(…)_LANG` is not a fence, a template, or an FFI call. The language tag is part of the expression. Nested blocks evaluate in their own runtimes; values cross as **OValue**, not as O(N²) pairwise bindings. The smoke source is exactly:

```O
python^(
__oval_result__ = 1 + 1
)_python
```

Expected result: `[number] 2`.

Environments are also syntax: `python^(…)_python` is ephemeral; `python[0]^(…)_python[0]` is a persistent actor; `python[*]^(…)_python[*]` is an explicit fresh-per-occurrence form.

---

## Two computation layers, deliberately not one

**Hosted orchestration (`.O`)** composes real language runtimes, persistent environments, deferred work, Nix values, and OS-as-participant values. Its IR is **OIR**. Serial OIR is the semantic oracle; the default executor is the **HGraph coordinator** (`O_EXECUTOR=serial` keeps the topological interpreter for differential checks).

The hosted pipeline, from `ARCHITECTURE.md`:

1. **Parse** typed expression trees (`src/parser.rs`)
2. **Lower** to OIR (`src/ir.rs`) — each `Exec` freezes backend identity, purity, splice renderer, dispatch mode
3. **Plan** an `ExecutionPlan` (structural, sequence, and data edges)
4. **Project/solve** a directed **HGraph** (values, resource versions, actor state, completion)
5. **Analyze and admit** — V5 evidence compiled into an immutable `AdmittedExecution`
6. **Schedule, render, authorize, dispatch** — coordinator accepts only that admission
7. **Settle and observe** — receipts/traces after execution, not instead of it

**O-core (`.oc`)** is a different compiler. It does not pretend kernel code is a notebook cell:

```text
.oc → AST → typed HIR → SSA MIR → freestanding ELF64
```

Primary target is **x86_64**. There is a conservative **AArch64** scalar subset (`aarch64-unknown-none`). The kernel proof is written so the *target image* does not depend on Python, JSON, subprocesses, a filesystem, libc, or Rust `std`. Those exist only in the hosted compiler and harness.

Hosted effects stay conservative. Native kernel claims have their own numbered gates (Modes 20–23 in `docs/CLAIMS.md`: nonexecuting VM objects, AMD SVM/NPT guest, QEMU-TCG administrative lifecycle, device-composition). Those are not silently inherited by `O file.O`.

---

## Three hosted implementations, one catalog

| Edition | Path | Role |
|---|---|---|
| Rust | `src/` | Authority. Default binary is `O`. |
| C17 | `c_cpp/` | Actively supported interpreter + `olangc` (Makefile and CMake, C17) |
| Python | `o_lang/` | Readable semantic reference |

The backend catalog is **one compile-time source**: `src/backend_catalog.inc.rs`, schema **`ostadix.backend-catalog/v4`**. Registry, native adapters, generated-runtime emission, and MCP runtime discovery are projections of that file. There is no second MCP backend table and no runtime catalog parser. V3 identities remain decodable but cannot authorize a current profile. There is no silent V3→V4 uplift.

Wire: 4-byte big-endian length prefix + canonical CBOR (`src/wire.rs`). Maps sorted by encoded key length then bytes.

On this machine, `o_env` reported:

- `O_LANG_ROOT=/Users/ustad/Ostadix-lang`
- `O` / `olangc` in `target/release`
- **30** catalog backends, **7** builtin, **22** located, **1** missing
- Python shim present

I did **not** run `o_smoke` this turn.

---

## What you can actually run from the tree

Rust package binaries (from `Cargo.toml`):

| Binary | Job |
|---|---|
| `O` | Interpret `.O`, REPL |
| `olangc` | AOT native host binary, WASI wasm, script, OIR, DOT |
| `ocorec` | O-core compiler |
| `o-link` / `o-unlink` | Lift a mixed tree into one route-aware artifact / restore it |
| `o-live-host` | Hosted Live-World package/store/supervisor oracle |
| `o-node` / `octl` | Hosted Placement V6 node + client (mTLS 1.3, V1 one-shot / V2 durable session) |
| `o-registry` | Local placement profiles and signed snapshots |
| `o-info` | Authority-free Information Kernel V1 local store / signed delta packs |
| `ogit` | O-Git / semantic receipts |
| `o-notebook` | Feature-gated (`notebook`) |
| `ocore-kernel-world-record` | Kernel-world record tooling |

The lowercase `o` wrapper (`scripts/o-cli.sh`) unifies `run`, `plan`, `why`, `node`, `live`, `receipt`, `kernel`, etc.

`olangc` AOT embeds a **snapshot of the Rust runtime at olangc build time** via `include_str!` plus `Cargo.lock`, then builds a temp Cargo project. An old `olangc` produces old AOT semantics.

`o-link` is sharp: a **bare directory** literal-links and **runs** selected executable blocks. `--project` is the inert route-preserving lift. `.olinkignore` exists for a reason.

MCP lives in `mcp/ostadix_lang_mcp_server` with its **own** `Cargo.lock` so `rmcp`/`tokio` stay out of the main crate. Tools: `o_env`, `o_doctor`, `o_runtimes`, `o_run`, `o_olangc`, `o_smoke`, `o_search_run`, plus the one-use same-intent pair `o_analyze_intent` / `o_execute_intent`. `o_run` is the ungated compatibility path; the intent pair recomputes the stable graph then does a **fresh V5 admission**.

Canonical rebuild is `./setup.sh`, not a lone `cargo build`.

---

## Hosted backends

Inline (evaluator-internal): `O`/`o`, `quote`, `html`, `markdown`/`md`, `latex`/`tex`, `text`/`plain`, `nix_expr`.

Shimmed / process-hosted (real runtimes, CBOR IPC): Python, Nix, nix_store, nixos_test, bash, shell, rust, racket, C, C++, C#, Haskell, Lisp / Common Lisp, SQL (SQLite), Ruby, MATLAB/Octave, Mathematica, WebAssembly, Java, JavaScript, OCaml. A few still use a legacy Python shim for semantics that are not “run this command” (live Python `O.eval` is the example).

Shims live in `backends/*_shim.py` plus `o_shim_common.py`. Missing runtime → explicit backend error, not “print the source and call it success.” `examples/manifest.json` classifies the example tree so an unsupported backend emitting literal text or a fatal shim diagnostic is **not** conformance, and an all-skipped sweep fails.

---

## The rest of the monorepo (research / experimental, not the hosted core)

These are real implementations with tests. “Experimental” here means narrower compatibility or operational guarantees, not “simulated.”

- **Admission V5** — supported local contract used by `O` and current MCP execution
- **Hosted Placement V6** — transport-independent descriptor / requirement / warrant / capacity / lease. V1: one fresh source document. V2: one locally prepared single-shim fragment + complete signed proof + durable explicitly-closed session. Admission version is **not** silently translated into a transport version
- **Information Kernel V1** + **BackendMorphism V1** — authority-free shadow surfaces; they do not authorize
- **Live-World** (`src/live_system/`) — bounded package store, default-deny activation, per-service supervision, generation-bound bearers. Host-process oracle, not a native O-core service manager
- **KernelWorld** (`src/kernel_world.rs`, `ocore.kernel-world/v1`) — strict manifest + host lifecycle oracle; decoding cannot recreate verified-package authority
- **World / O-Machine** — offline contracts and milestone proofs under `evidence/`
- **Project lifting** — `src/project/` ecosystems (Python, Rust, JS, Nix, Java, C family, …) plus logical HGraph / route execution

`docs/CLAIMS.md` is the evidence boundary. `docs/VERSIONING.md` is explicit that package SemVer, admission schema, catalog generation, transport, World codecs, and information records are **independent axes**. Matching numbers are coincidence.

---

## How the tree is laid out

```
src/           Rust authority (parser, OIR, eval, HGraph, evidence, placement, world, ocore, …)
ocore/         .oc runtime, kernel, world, user proofs
c_cpp/         C17 edition
o_lang/        Python reference
backends/      shims
examples/      .O programs (hello, nested splice, html/python, nix, sql, groups, …)
tests/         Rust integration + some Python-era tests
fuzz/          parser fuzz
mcp/           ostadix-mcp (separate crate)
docs/          CLAIMS, SPEC, SEMANTIC_CUSTODY, HOSTED_PLACEMENT_V6, OCORE, WORLD, …
evidence/      sealed gates, World/O-Machine records, transcripts
scripts/       setup, release builder, architecture-boundary check, smokes
setup.sh       canonical install
```

Architecture is still a **monolith crate** with the first cycle-breaking contracts (`syntax_dialect`, OIR not importing HGraph, `dispatch_model` as a pure classifier, identity below Effects/World). `scripts/check_architecture_boundaries.py` rejects wrong-way edges. It is not a Cargo workspace yet.

`docs/` is the design surface you actually use to bound speech: `CLAIMS.md`, `SPEC.md`, `SEMANTIC_CUSTODY.md`, `HOSTED_PLACEMENT_V6.md`, `INFORMATION_KERNEL_V1.md`, `OCORE.md`, `OSTADIX_WORLD.md`, `KERNEL_WORLD_CONTRACT.md`, `O_MACHINE_CONTRACT.md`.

---

## Maturity, as the README states it

Supported core: typed parentheses, OValue crossing, OIR / ExecutionPlan / HGraph / V5 admission / local executor (under hardening).

Experimental integration: Information Kernel, backend-morphism, Hosted V2 durable sessions, project lifting / live supervision.

Bounded research: O-core compiler and OKernel gates (exact QEMU nonclaims in CLAIMS). O-Machine and elastic governed World are a research direction with offline records.

Portability of **hosted** O: developed on macOS ARM64, Android ARM64 (rooted Pixel 8 Pro), Intel x86_64 Linux. Those native-target limits do not apply to `.O` execution.

Latest commit subject on this checkout: a local-first **Semantic Kernel Freeze** milestone — no lab, daemon, cloud, cluster, VM, or special hardware required for that milestone. I did not re-read the full commit body.

---

## One tree fact I should not hide

There is a **nested** `/Users/ustad/Ostadix-lang/Ostadix-lang/` directory that itself looks like another checkout (it has its own `.git`). I did not treat it as the canonical root; I did not inspect that nested git history. The live root is `/Users/ustad/Ostadix-lang`.

I also did not run examples, tests, or `setup.sh`. Description only, as you asked.

## User

explain what each imporatnt script does and how to perform every actoin 

## Assistant

I'll stay read-only and inventory the important scripts plus the command surfaces they implement, then walk through how each action is actually performed.

## Tools

- Read: /Users/ustad/.grok/skills/olang-runtime/SKILL.md
- ListDir: scripts
- ListDir: setup
- Read: /Users/ustad/Ostadix-lang/setup.sh (1-150)
- Read: /Users/ustad/Ostadix-lang/AGENTS.md
- IntegrationSearch (MCP tool discovery)

## Assistant

The script list is large. I’ll read the setup/CLI/test entrypoints first, then the release and evidence scripts so the “how to do it” matches the actual flags.

## Tools

- Read: /Users/ustad/.grok/skills/ostadix-term/SKILL.md
- Read: /Users/ustad/Ostadix-lang/scripts/o-cli.sh (1-133)
- Read: /Users/ustad/Ostadix-lang/boot-and-test.sh (1-200)
- Read: /Users/ustad/Ostadix-lang/setup/os/README.md
- Search: ^# |^usage|Usage:|function |^[a-zA-Z_]+\(\)|case
- Read: /Users/ustad/Ostadix-lang/README.md (1601-1720)
- Read: /Users/ustad/Ostadix-lang/scripts/o-kernel.sh (1-150)
- Read: /Users/ustad/Ostadix-lang/scripts/install-o-cli-wrapper.sh (1-33)
- Read: /Users/ustad/Ostadix-lang/test_o_lang_examples.sh (1-18)
- Read: /Users/ustad/Ostadix-lang/tests/test_cli.sh (1-80)
- Read: /Users/ustad/Ostadix-lang/scripts/build_source_release.py (1-80)
- Read: /Users/ustad/Ostadix-lang/scripts/release_evidence.py (1-100)
- Read: /Users/ustad/Ostadix-lang/scripts/check_release_claims.sh (1-60)
- Read: /Users/ustad/Ostadix-lang/scripts/check_declared_bins.sh (1-60)
- Read: /Users/ustad/Ostadix-lang/scripts/check_architecture_boundaries.py (1-50)
- Read: /Users/ustad/Ostadix-lang/scripts/contract_surfaces.py (1-60)
- Read: /Users/ustad/Ostadix-lang/scripts/local_ci_posture.py (1-60)
- Read: /Users/ustad/Ostadix-lang/scripts/smoke_ostadix_mcp.py (1-60)
- Read: /Users/ustad/Ostadix-lang/scripts/world_alpha_evidence.py (1-70)
- Read: /Users/ustad/Ostadix-lang/scripts/smoke-hosted-live-reference.sh (1-50)
- Read: /Users/ustad/Ostadix-lang/scripts/smoke-project-hgraph.sh (1-40)
- Read: /Users/ustad/Ostadix-lang/scripts/smoke-project-hgraph-exec.sh (1-22)
- Read: /Users/ustad/Ostadix-lang/scripts/smoke-world-g0-conformance.sh (1-40)
- Read: /Users/ustad/Ostadix-lang/scripts/smoke-world-resource-keys.sh (1-40)
- Read: /Users/ustad/Ostadix-lang/scripts/smoke-docker.sh (1-50)
- Read: /Users/ustad/Ostadix-lang/scripts/semantic_custody_demo.sh (1-40)
- Read: /Users/ustad/Ostadix-lang/scripts/demo_o_link_schedule_why.sh (1-40)
- Read: /Users/ustad/Ostadix-lang/scripts/benchmark_hgraph_hosted.sh (1-50)
- Read: /Users/ustad/Ostadix-lang/scripts/big_iron_to_my_texas_red.sh (1-50)
- Read: /Users/ustad/Ostadix-lang/scripts/ostadix_boot_media.py (1-50)
- Read: /Users/ustad/Ostadix-lang/scripts/ostadix_media_writer.py (1-50)
- Read: /Users/ustad/Ostadix-lang/scripts/ostadix_physical_evidence.py (1-50)
- ListDir: ocore/kernel
- Search: ^## Running the tests|^### |^## Quickstart|^## Getting-started|^## Hosted
- Read: /Users/ustad/Ostadix-lang/README.md (1706-1795)
- Read: /Users/ustad/Ostadix-lang/README.md (3786-3965)
- Read: /Users/ustad/Ostadix-lang/README.md (2191-2270)
- Read: /Users/ustad/Ostadix-lang/README.md (2761-2840)
- Read: /Users/ustad/Ostadix-lang/scripts/o-kernel.sh (149-234)
- Read: /Users/ustad/Ostadix-lang/ocore/kernel/build.sh (1-40)
- Read: /Users/ustad/Ostadix-lang/ocore/kernel/run-qemu.sh (1-40)
- Read: /Users/ustad/Ostadix-lang/ocore/kernel/smoke-qemu.sh (1-40)
- Read: /Users/ustad/.config/ostadix/term/ostadix-term.zsh (1-200)
- Search: install_wrappers|build_mcp|verify_hosted|RUST_BIN
- Read: /Users/ustad/Ostadix-lang/README.md (545-624)
- Read: /Users/ustad/Ostadix-lang/README.md (1206-1255)
- Search: add_argument|subparsers|argparse
- Search: add_argument|subparsers
- Search: ^o\(\)|o_help|o pull|o build|o doctor|o smoke|o live
- Read: /Users/ustad/.config/ostadix/term/ostadix-term.zsh (201-450)
- Read: /Users/ustad/Ostadix-lang/setup.sh (851-930)
- Read: /Users/ustad/Ostadix-lang/boot-and-test.sh (201-267)
- Read: /Users/ustad/.config/ostadix/term/ostadix-term.zsh (501-680)
- Read: /Users/ustad/.config/ostadix/term/ostadix-term.zsh (901-1080)
- Execute: export PATH="/Users/ustad/Ostadix-lang/target/release:$HOME/.local/bin:$PATH"
for b in O olangc o-link o-unlink ocorec o-live-host o-node octl o-registry o-info ogit; do
  echo "======== $b --help ========"
  if [ -x "/Users/ustad/Ostadix-lang/target/release/$b" ]; then
    "/Users/ustad/Ostadix-lang/target/release/$b" --help 2>&1 | head -80
  else
    echo "MISSING"
  fi
  echo
done
echo "======== o-cli.sh help ========"
/Users/ustad/Ostadix-lang/scripts/o-cli.sh help 2>&1 | head -40
echo "======== o-kernel.sh help ========"
/Users/ustad/Ostadix-lang/scripts/o-kernel.sh help 2>&1 | head -50
echo "======== o-live-host help ========"
/Users/ustad/Ostadix-lang/target/release/o-live-host --help 2>&1 | head -60
echo "======== octl node help ========"
/Users/ustad/Ostadix-lang/target/release/octl --help 2>&1 | head -50
echo "======== o-node help ========"
/Users/ustad/Ostadix-lang/target/release/o-node --help 2>&1 | head -60
echo "======== o-info help ========"
/Users/ustad/Ostadix-lang/target/release/o-info --help 2>&1 | head -50
echo "======== o-registry help ========"
/Users/ustad/Ostadix-lang/target/release/o-registry --help 2>&1 | head -50
echo "======== ogit help ========"
/Users/ustad/Ostadix-lang/target/release/ogit --help 2>&1 | head -50
 (Collect --help from release CLIs)
- Execute: export PATH="/Users/ustad/Ostadix-lang/target/release:$PATH"
echo '======== octl node --help ========'
/Users/ustad/Ostadix-lang/target/release/octl node --help 2>&1 | head -80
echo
echo '======== octl node session --help ========'
/Users/ustad/Ostadix-lang/target/release/octl node session --help 2>&1 | head -50
echo
echo '======== octl node authority --help ========'
/Users/ustad/Ostadix-lang/target/release/octl node authority --help 2>&1 | head -40
echo
echo '======== o-node serve --help ========'
/Users/ustad/Ostadix-lang/target/release/o-node serve --help 2>&1 | head -40
echo
echo '======== o-live-host demo --help ========'
/Users/ustad/Ostadix-lang/target/release/o-live-host demo --help 2>&1 | head -30
echo
echo '======== o-info debug ========'
ls /Users/ustad/Ostadix-lang/target/release/o-info /Users/ustad/Ostadix-lang/target/debug/o-info 2>&1
echo
echo '======== world_alpha / contract / local_ci / physical help ========'
python3 /Users/ustad/Ostadix-lang/scripts/world_alpha_evidence.py --help 2>&1 | head -40
python3 /Users/ustad/Ostadix-lang/scripts/contract_surfaces.py --help 2>&1 | head -40
python3 /Users/ustad/Ostadix-lang/scripts/local_ci_posture.py --help 2>&1 | head -40
python3 /Users/ustad/Ostadix-lang/scripts/ostadix_physical_evidence.py --help 2>&1 | head -40
python3 /Users/ustad/Ostadix-lang/scripts/ostadix_boot_media.py --help 2>&1 | head -40
python3 /Users/ustad/Ostadix-lang/scripts/ostadix_media_writer.py --help 2>&1 | head -40
python3 /Users/ustad/Ostadix-lang/scripts/check_architecture_boundaries.py --help 2>&1 | head -20
 (Collect remaining CLI and script help)
- Read: /Users/ustad/Ostadix-lang/setup.sh (1541-1620)
- Search: verify_runnable|function verify
- Read: /Users/ustad/Ostadix-lang/README.md (801-880)

## Assistant

Lee, this is the operator map of the tree: every important script, what it actually does, and the exact command for each action. I did not change anything. I read the scripts and `--help` of the release binaries that exist; I did **not** run smokes, tests, setup, or kernel boots.

Two facts first, or the rest will lie.

**There are two `o` surfaces.** Your zsh kit (`~/.config/ostadix/term/ostadix-term.zsh`) owns `o pull`, `o doctor`, `o ship`, `o live demo`, etc. The repository dispatcher (`scripts/o-cli.sh`, installed as `~/.local/bin/o`) owns `run / plan / why / node / node-host / registry / info / live / receipt / kernel`. Uppercase `O` is always the evaluator. On a case-insensitive disk those two names collide; the wrapper inspects `$0` so `O` still hits `target/release/O` and `o` still hits `o-cli.sh`. If a command is missing, you are on the wrong `o`.

**`o-info` is declared and wired, but it is not on this checkout’s `target/release`.** `o-cli.sh info` will fail until that binary is built (via `setup.sh`, not a one-off `cargo` in the live tree while you are developing).

Canonical env:

```bash
export O_LANG_ROOT=/Users/ustad/Ostadix-lang
export O_BACKENDS_DIR="$O_LANG_ROOT/backends"
export PATH="$HOME/.local/bin:$O_LANG_ROOT/target/release:$PATH"
source "$HOME/.config/ostadix/env.sh"   # written by setup.sh
```

Never put `$VAR` inside a `.O` file. Always pass an **absolute** backends path to `O`.

---

## 1. Bootstrap and install

### `setup.sh` — the canonical installer

This is the one script that is supposed to make the machine match the tree. It installs selected OS deps, writes `~/.config/ostadix/env.sh`, builds the locked Rust bins, the C17 edition, the MCP server, checks the Python reference, installs wrappers, then optionally verifies.

What it builds by default (`RUST_BIN_TARGETS`):  
`O`, `olangc`, `ocorec`, `o-link`, `o-unlink`, `ogit`, `o-live-host`, `o-node`, `octl`, `o-registry`, `o-info`.  
`--full` also builds `o-notebook`.

How to do it:

```bash
cd "$O_LANG_ROOT"

./setup.sh -h                         # flags
./setup.sh --dry-run                  # print the plan, change nothing
./setup.sh --check                    # capability probe only
./setup.sh --deps-only                # deps + env, no Ostadix build
./setup.sh -y --minimal               # hosted core + MCP + wrappers
./setup.sh -y --minimal --verify      # same, then hello.O on Rust/C17/Python/AOT
./setup.sh -y --full                  # notebook + Nix + WASI + O-core tools
./setup.sh --with-nix
./setup.sh --no-nix                   # even under --full
./setup.sh --with-ocore               # clang, lld, qemu-system-x86_64 / aarch64
./setup.sh --with-ocore-media         # also GRUB/FAT/UEFI media tools
./setup.sh --with-hosted-runtimes     # extra language runtimes (no Java/licensed)
./setup.sh --with-linux-kernel-tools  # Linux host only
./setup.sh --with-guest-tools
./setup.sh --with-ubuntu-vm           # Multipass for ubuntu_vm^
./setup.sh --with-ocore --verify-ocore
./setup.sh --no-wrappers
./setup.sh --no-mcp
./setup.sh --persist-env              # source env.sh from the current shell rc
./setup.sh --env-file PATH
./setup.sh --no-env
```

From the zsh kit (this **does** `setup.sh`, not bare cargo):

```bash
o pull                    # fetch + ff-only + ./setup.sh -y --minimal
o pull --full
o pull --verify
o pull --smoke            # then hello.O → 2
o pull --rebase
o pull --force-reset      # discard local work; match origin
o pull --no-wrappers
o update                  # alias of o pull
o build                   # setup.sh -y --minimal, no git
o build --full
o build --verify
o build --cargo           # explicit escape hatch: cargo build --release
```

`o pull --force-reset` is destructive. I am stating that, not recommending it.

### `setup/os/setup-*.sh`

Thin delegates. Every option is forwarded unchanged to root `setup.sh`. They exist so old platform commands keep working.

```bash
./setup/os/setup-macos.sh -y --minimal --verify
./setup/os/setup-debian.sh -y --minimal
# alpine, arch, fedora, freebsd, gentoo, nixos, opensuse, tinycore, void, windows
```

Windows script recommends WSL + the Debian entrypoint.

### `scripts/install-o-cli-wrapper.sh`

Writes one file. If `$0` is `O`, exec `target/release/O`. Otherwise exec `scripts/o-cli.sh`.

```bash
bash scripts/install-o-cli-wrapper.sh "$HOME/.local/bin/o"
o install-wrappers          # term kit: O, o, olangc, o-link, o-unlink, ocorec
```

---

## 2. The two `o` dispatchers

### `scripts/o-cli.sh` — repository-owned lowercase `o`

This is what `~/.local/bin/o` is supposed to be.

```bash
o help
o run FILE.O [backends]          # sets O_BACKENDS_DIR, execs O
o plan <project-or-.O> [opts]    # olangc $1 --target ir
o why FILE.O P3 [opts]           # olangc --target ir --why P3
o node …                         # octl node …
o node-host …                    # o-node …
o registry …                     # o-registry …
o info …                         # o-info …  (binary missing here)
o live …                         # o-live-host …
o receipt                        # ogit demo semantic-receipt
o receipt <ogit args>
o kernel …                       # scripts/o-kernel.sh
o anything-else                  # forwarded to O (historical lowercase evaluator)
```

### Term kit `o` — richer, zsh-only

Extra verbs that **do not** exist on `o-cli.sh`: `doctor`, `pull`, `build`, `smoke`, `ship`, `graph`, `e`, `repl`, `link-project`, `link-here`, `test`, `new`, `ex`, `semdiff`, `mir`/`hir`/`asm`/`obj`, `ssh`, showreel commands.

If you are in a non-zsh agent or a raw PATH wrapper, use the explicit binaries or `scripts/o-cli.sh`.

---

## 3. Hosted `.O` — interpret, plan, compile

### `O` — interpreter / REPL

```bash
O examples/hello.O "$O_LANG_ROOT/backends"     # expect [number] 2
O --eval 'python^(__oval_result__ = 1+1)_python'
O --json examples/hello.O
O --check examples/hello.O
O --repl "$O_LANG_ROOT/backends"
O --executor serial examples/hello.O "$O_LANG_ROOT/backends"
O --executor graph --workers 4 examples/hello.O "$O_LANG_ROOT/backends"
O --require-source-sha256 HEX --require-execution-intent-sha256 HEX file.O backends
O version --json
```

Term kit:

```bash
o run file.O
o e 'python^(__oval_result__ = 1+1)_python'
o repl
o smoke
```

Python / C17 editions of the same program:

```bash
python3 -m o_lang examples/hello.O
make -C c_cpp
c_cpp/O examples/hello.O "$O_LANG_ROOT/backends"
```

### `olangc` — plan / script / AOT / wasm / DOT

Nothing in `--target ir` or `--target dot` executes user backends.

```bash
olangc file.O --target ir --shim-dir "$O_LANG_ROOT/backends"
olangc file.O --target ir --explain-schedule --shim-dir …
olangc file.O --target ir --why P3 --shim-dir …
olangc file.O --target ir --execution-intent-json --shim-dir …
olangc file.O --target ir --grounding --world-id ID --world-epoch N --shim-dir …
olangc file.O --target dot --shim-dir … | dot -Tpng -o graph.png
olangc file.O --target script --shim-dir …
olangc file.O -o target/app --shim-dir …              # native AOT (default)
olangc file.O --target wasm -o target/app.wasm --shim-dir …
olangc file.O -o target/app --keep-build-dir --shim-dir …
```

For a directory or a lifted project, `ir`/`dot` print the **project** plan/HGraph, not a hosted OIR dump.

Term kit:

```bash
o plan file.O
o why file.O P3
o graph file.O
o ir file.O
o script file.O
o bin file.O [out]          # AOT
o wasm file.O
o ship file.O [out]         # interpret → plan → script → AOT
```

AOT embeds a **snapshot of the Rust runtime at olangc build time**. If an AOT binary behaves like an old runtime, rebuild `olangc` via `setup.sh` / `o pull`, then recompile the program.

### `o-link` / `o-unlink`

A **bare directory is not inert**. It literal-links selected UTF-8 files and **runs** them. Use `.olinkignore`. `--project` is the safe lift.

```bash
o-link src/                              # writes combined.O and RUNS it
o-link src/ --literal -o sequential.O    # link only
o-link src/ --literal --run -o seq.O     # link then run
o-link --project . -o target/project.O   # inert route-preserving bundle
o-link --list-routes target/project.O
o-link --project . --route ID --run
o-link --parallel calc.py report.py -o parallel.O
o-link --parallel=verified page.html -o v.O
o-link --parallel --explain-parallel a.py b.py
o-link a.py b.html -o out.O --verbose-skips --shebang

o-unlink out.O -o restored/ --dry-run
o-unlink out.O -o restored/
```

Term kit:

```bash
o link DIR
o link-project DIR
o link-here                    # literal-link + run cwd → ./linked.O
o unlink FILE.O
```

Demo of link + admitted schedule + `o why` + serial-vs-graph compare:

```bash
bash scripts/demo_o_link_schedule_why.sh
bash scripts/demo_o_link_schedule_why.sh --workers 4 --output-dir target/tmp/why
```

It never deletes its output directory.

### Source → custody artifacts

```bash
bash scripts/semantic_custody_demo.sh
# or
OUT=target/semantic-custody
olangc examples/semantic_custody.O --target ir --execution-intent-json --shim-dir backends > "$OUT/execution-intent.json"
olangc examples/semantic_custody.O --target ir --explain-schedule --shim-dir backends > "$OUT/schedule.txt"
olangc examples/semantic_custody.O --target dot --shim-dir backends > "$OUT/graph.dot"
```

---

## 4. Live-World (`o-live-host`)

Hosted semantic oracle: immutable SHA-256 package store, default-deny policy, per-service children, health-gated publish, generation-bound bearers, rollback, restart, reconstruction. Host processes, not O-core.

```bash
o-live-host --help
o-live-host demo --state /tmp/live-demo
o-live-host pack PATH
o-live-host install PATH --state DIR
o-live-host activate DIGEST --state DIR
o-live-host upgrade … --state DIR
o-live-host invoke SERVICE --state DIR
o-live-host compose … --state DIR
o-live-host rollback --state DIR
o-live-host restart SERVICE --state DIR
o-live-host status --state DIR
```

`--policy FILE` is exact-match TOML. Omitted = default deny.

The official acceptance gate (CAS, denied over-broad cap, health-gated activate, cross-world OValue, failed-upgrade rollback, stale bearer, crash isolation, reconstruct):

```bash
bash scripts/smoke-hosted-live-reference.sh
```

Term kit (state defaults to `~/.config/ostadix/live`):

```bash
o live demo                 # temp dir, then deleted
o live demo "$O_LIVE_STATE" # persist
o live status
o live smoke                # the official script
o live status --state DIR   # raw passthrough if you already pass --state
```

`o-cli.sh live` is the raw binary, not the pretty demo wrapper.

---

## 5. Hosted Placement V6 — `o-node` + `octl`

Transport-independent placement. V1 = one fresh source document. V2 = durable session, one prepared shim fragment, signed lease, explicit close. mTLS 1.3 both ways. Development walkthrough is in the README; it is **not** production enrollment.

**Host (via `o node-host` / `o-node`):**

```bash
o-node pki init --directory "$PKI" --server-name localhost
o-node identity init --state-dir "$STATE"
o-node profile …
o-node doctor --shim-dir "$O_LANG_ROOT/backends" --cert … --key … --client-ca …
o-node serve \
  --node-id demo-node \
  --shim-dir "$O_LANG_ROOT/backends" \
  --runtime-binary "$O_LANG_ROOT/target/release/O" \
  --bind 127.0.0.1:7337 \
  --cert "$PKI/node-cert.pem" \
  --key "$PKI/node-key.pem" \
  --client-ca "$PKI/ca.pem" \
  --v2-state-dir "$STATE" \
  --v2-authority-public-key "$AUTH/placement-public-key.v2"
o-node admin gc-closed --state-dir "$STATE"
```

**Client (via `o node` / `octl node`):**

```bash
octl node profile --address 127.0.0.1:7337 --server-name localhost --ca … --cert … --key …
octl node doctor  …

octl node authority init --directory "$AUTH"
octl node authority dev-mint open    --submit …   # Open lease + capability
octl node authority dev-mint execute --submit …   # first execute
octl node authority issue …                       # mint one PlacementLeaseV2

octl node session principal …
octl node session open …
octl node session exec …
octl node session status --operation-id demo-operation …
octl node session actors …
octl node session reset …
octl node session recover …
octl node session close …

octl node run …     # V1 one-shot: one exact source on one explicit node
```

Development capacity observations expire in **four seconds**. The README walkthrough uses `--submit` so mint and send are one command.

---

## 6. Local registry and information store

```bash
o-registry init
o-registry profile-local
o-registry publish-profile FILE.json
o-registry verify
o-registry list
o-registry export
o-registry import FILE
```

`o-info` (when the binary exists): local authority-free Information Kernel V1 head and signed offline delta packs. On this checkout I looked: **`target/release/o-info` is absent**. `o-cli.sh info` cannot work until `setup.sh` (or an isolated build) produces it.

---

## 7. O-Git

Narrow demo surface, not a general VCS.

```bash
ogit demo semantic-receipt
ogit diff-semantic a.O b.O
o receipt                 # both dispatchers
o semdiff a.O b.O         # term kit only
```

Receipt artifacts: `examples/group_pipeline/generated/` and `.ogit/receipts/semantic-receipt-001.json`.

---

## 8. O-core and the kernel

### `ocorec`

Different pipeline. Not OIR.

```bash
ocorec ocore/examples/minimal.oc --emit hir -o -
ocorec ocore/examples/minimal.oc --emit mir -o -
ocorec ocore/examples/minimal.oc --emit asm -o -
ocorec ocore/examples/minimal.oc --emit obj --keep-asm -o target/minimal.o
ocorec ocore/examples/minimal.oc --target aarch64-unknown-none --emit obj -o target/minimal-aarch64.o
```

Term kit: `o hir`, `o mir`, `o asm`, `o obj`, `o oc list`.

If `rust-lld` is off PATH: `export OCORE_LLD=/absolute/path/to/ld.lld`.

### `scripts/o-kernel.sh` — `o kernel`

Delegates to `ocore/kernel/*.sh` and the media/physical Python tools. Does not invent a second build.

```bash
o kernel doctor            # setup.sh --with-ocore --check --no-env
o kernel doctor-media      # setup.sh --with-ocore-media --check --no-env
o kernel build             # OCORE_PROBE_MODE=0 ocore/kernel/build.sh
o kernel image             # rebuild + bytes + sha256
o kernel media [path]      # GPT/UEFI disk image
o kernel inspect-media [img]
o kernel prepare-write --image IMG --device DEV
o kernel write-media …     # confirmation-token gated; writes a real device
o kernel boot-challenge
o kernel prepare-physical …
o kernel record-physical …
o kernel boot              # interactive QEMU, probe 0; quit Ctrl-A X
o kernel boot-media        # OVMF, no -kernel
o kernel console           # probe 16, native M5 `o> `
o kernel smoke
o kernel smoke-media
o kernel smoke-boot-info
o kernel smoke-smp
o kernel smoke-live
o kernel gates             # boot-and-test.sh smoke (all required portable gates)
```

`write-media` is the one script that can destroy a disk. `prepare-write` binds a token to **that** image and **that** device; `write` refuses a mismatched token.

### Kernel scripts those commands call

| Script | Action |
|---|---|
| `ocore/kernel/build.sh` | `ocorec` + `boot.S` + LLD → `target/ocore-kernel/kernel.elf`. `OCORE_PROBE_MODE` 0–34 selects which `.oc` corpus is compiled in. |
| `ocore/kernel/run-qemu.sh` | Interactive serial QEMU. Only modes **0** and **16**. |
| `ocore/kernel/build-x86_64-uefi-media.sh` | Deterministic GPT/ESP image |
| `ocore/kernel/run-x86_64-uefi-media-qemu.sh` | Boot that image under OVMF |
| `ocore/kernel/resolve-x86_64-ovmf-code.sh` | Find firmware |
| `ocore/kernel/build-aarch64-g2.sh` | G2 AArch64 objects |
| `ocore/kernel/build-m4-artifacts.sh` … `build-m7b-*.sh` | Milestone fixture images (loader, live, personality, Linux/9P, logical-read) |
| `ocore/kernel/check-m5-control.sh` | M5 console contract without claiming a full live system |
| `ocore/kernel/check-linux-minimal-slice.sh` | Bounded Linux-personality static check |
| `ocore/kernel/build-linux-minimal-corpus.sh` | Build that slice’s corpus |

**Smoke / evidence gates** (each asserts a fixed serial transcript; success is marker-exact, not “QEMU exited 0”):

| Script | What it proves (bounded) |
|---|---|
| `smoke-qemu.sh` | Baseline M0.1–0.3: CPL3, SYSCALL, IRQ0, heartbeat, W^X, caps |
| `smoke-faults-qemu.sh` | User-copy fault |
| `smoke-scheduler-qemu.sh` | M2 scheduler |
| `smoke-ipc-foundation-qemu.sh` / `smoke-ipc-qemu.sh` | M3 IPC |
| `smoke-loader-qemu.sh` | M4 loader / OVFS |
| `smoke-live-qemu.sh` / `smoke-live-semantics-qemu.sh` | M5 live + semantics |
| `smoke-personality-qemu.sh` | M6A personality |
| `smoke-m6b-qemu.sh` | M6B memory/resource revocation |
| `smoke-kernel-world-qemu.sh` | Mode 20: verified world, **nonexecuting** VM objects |
| `smoke-kernel-world-execution-qemu.sh` | Mode 21: **AMD + KVM + nested SVM**. Not in the portable aggregate |
| `smoke-kernel-world-live-qemu.sh` | Mode 22 TCG KernelWorld lifecycle |
| `smoke-kernel-world-execution-device-qemu.sh` | Mode 23 TCG SVM + one PIO endpoint |
| `smoke-live-bounded-personality-qemu.sh` | Mode 24 four-byte personality RPC |
| `smoke-live-linux-personality-qemu.sh` | Mode 25 static Linux ELF / CPL3 |
| `smoke-live-linux-plan9-qemu.sh` | Mode 26 Linux→9P2000 composition |
| `smoke-world-identity-qemu.sh` | OWIDENT |
| `smoke-world-protocol-qemu.sh` | OWPROTO |
| `smoke-world-value-qemu.sh` | OWVALUE |
| `smoke-world-receipt-qemu.sh` | OWRECEIPT |
| `smoke-world-project-runtime-qemu.sh` / `smoke-world-project-receipt-qemu.sh` | Project/runtime World receipts |
| `smoke-m7b-logical-read-qemu.sh` | Logical-read slice |
| `smoke-x86_64-uefi-media-qemu.sh` | Deterministic media rebuild + UEFI boot |
| `smoke-x86_64-boot-info-qemu.sh` | Firmware handoff / challenged mode-0 |
| `smoke-x86_64-smp-qemu.sh` | Four-vCPU INIT/SIPI + one barrier. Not general SMP |
| `smoke-aarch64-g2-qemu.sh` | G2 AArch64 TCG (EL2/EL1/EL0). Not physical ARM |

Direct:

```bash
./ocore/kernel/smoke-qemu.sh
OCORE_PROBE_MODE=16 OCORE_BUILD_DIR=target/ocore-m5-native ./ocore/kernel/build.sh
```

---

## 9. The phased “do everything” entry

### `boot-and-test.sh`

One sequencer. Missing optional tools skip rather than abort, except the required-gate manifest in `smoke`.

```bash
./boot-and-test.sh            # default: quick
./boot-and-test.sh setup      # ./setup.sh --minimal --verify
./boot-and-test.sh hosted     # hello.O graph + serial + python + C17
./boot-and-test.sh ocore      # ocorec hir/mir/obj
./boot-and-test.sh kernel     # build + interactive QEMU (does not return)
./boot-and-test.sh smoke      # every required gate in evidence/gates.toml
./boot-and-test.sh tests      # cargo test + proptest + reproducibility + examples
./boot-and-test.sh quick      # hosted + ocore + smoke-qemu.sh only
./boot-and-test.sh full       # setup + hosted + ocore + all gates + tests
```

`okernel-multikernel/boot-and-test.sh` is the same entry from that subdirectory; it walks up to find `Cargo.toml` + `ocore/kernel/build.sh`.

`smoke` does **not** trust a green exit. It `tee`s each gate and then:

```bash
python3 scripts/release_evidence.py verify-transcript --script GATE --transcript FILE
```

A deleted required script makes the aggregate **redder**, not greener.

---

## 10. Evidence, claims, release, CI posture

### `scripts/release_evidence.py`

Reads `evidence/gates.toml` (schema 2). **26** required portable gates, **1** supplemental hardware gate.

```bash
python3 scripts/release_evidence.py validate
python3 scripts/release_evidence.py list-scripts
python3 scripts/release_evidence.py verify-transcript --script PATH --transcript FILE
python3 scripts/release_evidence.py project            # check generated README/CI blocks
python3 scripts/release_evidence.py project --write    # only if you intend to update projections
```

### `scripts/world_alpha_evidence.py`

Validates the OSTADIX Alpha G0–G13 registry (`evidence/world_alpha_gates.toml`). Separate from the executable O-core gate list.

```bash
python3 scripts/world_alpha_evidence.py
python3 scripts/world_alpha_evidence.py --definitions-only --quiet
python3 -m unittest -v tests.test_world_alpha_evidence
```

### `scripts/check_release_claims.sh`

Prose/source guard against stale implementation claims. First validates both evidence registries, then scans tracked files for banned phrases.

```bash
bash scripts/check_release_claims.sh
```

### `scripts/check_declared_bins.sh`

`cargo metadata` + `cargo check --bins --all-features` and asserts every `[[bin]]` produced an artifact. **This is a Cargo invocation.** Do it in `moral-gaur` or a throwaway worktree, not the live macOS tree while you are developing.

### `scripts/check_architecture_boundaries.py`

Lexical reject of the first frozen wrong-way Rust edges (parser↛IR, IR↛HGraph, effects↛World, …).

```bash
python3 scripts/check_architecture_boundaries.py
```

### `scripts/contract_surfaces.py`

Keeps duplicated CI/build literals honest against `ci/*.toml`, the backend catalog, and workflows.

```bash
python3 scripts/contract_surfaces.py validate
python3 scripts/contract_surfaces.py required-executables
python3 scripts/contract_surfaces.py probe-runtimes --suite rust-hosted
python3 scripts/contract_surfaces.py probe-runtimes --suite docker
```

A successful probe means “this command is invocable.” It is not a World, a placement, or a healthy backend.

### `scripts/local_ci_posture.py`

Read-only CI/supply-chain audit. Never installs tools. Full profile may run cargo-deny against a **temp mirror**.

```bash
python3 scripts/local_ci_posture.py --profile baseline --format text
python3 scripts/local_ci_posture.py --profile full --format json
python3 scripts/local_ci_posture.py --github     # gh GET only
```

### `scripts/build_source_release.py`

Builds a deterministic ZIP from **Git objects at a commit**, never the dirty worktree. Allowlisted top-level surfaces only.

```bash
python3 scripts/build_source_release.py --output target/release-validation/Ostadix-lang-source.zip
python3 scripts/build_source_release.py --verify target/release-validation/Ostadix-lang-source.zip
python3 scripts/build_source_release.py --ref HEAD --output dist/x.zip
python3 -m unittest -v tests.test_source_release
```

`--allow-dirty` lets you run on a dirty tree; it still packages committed bytes only.

### Media / physical (also reached via `o kernel`)

```bash
python3 scripts/ostadix_boot_media.py pack …
python3 scripts/ostadix_boot_media.py inspect IMG
python3 scripts/ostadix_boot_media.py plan-target …

python3 scripts/ostadix_media_writer.py prepare --image IMG --device DEV
python3 scripts/ostadix_media_writer.py write --token TOKEN …

python3 scripts/ostadix_physical_evidence.py challenge --raw
python3 scripts/ostadix_physical_evidence.py prepare …
python3 scripts/ostadix_physical_evidence.py verify …
python3 scripts/ostadix_physical_evidence.py check-transcript …
```

`ostadix_boot_media.py` never writes a device. The writer does, and only after a bound confirmation token.

---

## 11. Hosted tests and examples

### `test_o_lang_examples.sh`

Runs `tests/example_manifest.py` against the Rust `O` for every example classified `unit` or `integration`. Unsupported backends that print literal source, or a sweep that skips everything, fail.

```bash
bash test_o_lang_examples.sh
o test-examples             # term kit
```

### `tests/test_cli.sh`

Release-CLI contract: `O` errors and success, `olangc` native output, `ocorec` ELF object, `o-link`/`o-unlink`/`o-cli`/`o-kernel` help. Requires the **release** bins already present.

```bash
bash tests/test_cli.sh
o test-cli
```

### Other hosted smokes

```bash
bash scripts/smoke-project-hgraph.sh          # project lift / plan (mostly cargo tests)
bash scripts/smoke-project-hgraph-exec.sh     # ordered hosted project execution
bash scripts/smoke-world-g0-conformance.sh    # G0 contract hashes + World rust tests
bash scripts/smoke-world-resource-keys.sh     # resource-key World tests
bash scripts/smoke-docker.sh                  # build image, hello, project lift, literal o-link
# OSTADIX_DOCKER_SKIP_BUILD=1 OSTADIX_DOCKER_IMAGE=o-lang:0.2.0 bash scripts/smoke-docker.sh
```

The two project/world smokes call `cargo test`. Same isolation rule.

### `scripts/benchmark_hgraph_hosted.sh`

Descriptive. No pass/fail threshold.

```bash
bash scripts/benchmark_hgraph_hosted.sh
bash scripts/benchmark_hgraph_hosted.sh --shape realistic --workers 4 --repetitions 5
```

Shapes: `heterogeneous`, `chained`, `mixed_width`, `realistic`, or `all`.

### `scripts/smoke_ostadix_mcp.py`

Real stdio MCP transport against a built `ostadix-mcp`. Checks exact tool set, `o_env`, `o_runtimes`, `o_smoke`, relative `o_run`, `o_olangc`. CI launches the server with a **system-only PATH** so discovery cannot hide a broken runtime path.

```bash
python3 scripts/smoke_ostadix_mcp.py
```

---

## 12. MCP (agent path)

Built by `setup.sh` unless `--no-mcp`. Separate crate, own lockfile.

```bash
# isolated / Multipass, not the live tree while you are developing:
cargo build --release --locked --manifest-path mcp/ostadix_lang_mcp_server/Cargo.toml
```

Tools: `o_env`, `o_doctor`, `o_runtimes`, `o_smoke`, `o_run`, `o_olangc`, `o_search_run`, `o_analyze_intent`, `o_execute_intent`.

`o_run` is ungated compatibility execution. The intent pair is: analyze without running → one-use handle → recompute the same intent → **fresh V5 admission** → dispatch.

`.mcp.json` registers `ostadix-mcp`. After setup: `ostadix-mcp` on `~/.local/bin`.

---

## 13. Docker

```bash
# image used by the README examples
docker build -t o-lang:0.2.0 .
docker run --rm o-lang:0.2.0                       # image hello path
bash scripts/smoke-docker.sh                       # official gate
```

`docker --version` is not enough; the smoke requires `docker info`.

---

## 14. Notebook, C17-only, Python-only

```bash
# after setup.sh --full (notebook feature)
cargo run --features notebook --bin o-notebook -- backends
# listens 127.0.0.1:8888

# C17 only
cd c_cpp && make
./O ../examples/hello.O ../backends
./olangc ../examples/hello.O -o /tmp/h && /tmp/h
make test

# Python reference
python3 -m o_lang examples/hello.O
python3 -m tests.test_parser
python3 -m tests.test_evaluator
```

Wrappers from setup: `o-c` / `olangc-c` point at the C17 bins.

---

## 15. Tests the README treats as the real gate

On **this** Mac, Cargo belongs in Multipass `moral-gaur` (`CARGO_TARGET_DIR=/home/ubuntu/ostadix-target…`) or a throwaway clone. A Multipass socket failure is not a source failure.

```bash
multipass list
multipass exec moral-gaur -- bash -lc 'cd /home/ubuntu/Ostadix-lang && git pull --ff-only origin master && git rev-parse HEAD'
multipass exec moral-gaur -- bash -lc '
  set -euo pipefail
  cd /home/ubuntu/Ostadix-lang
  export CARGO_TARGET_DIR=/home/ubuntu/ostadix-target-hosted-v2
  export OSTADIX_TEST_RUNTIME_POLICY=required
  rustup run 1.97.1 cargo check --locked --all-targets --all-features
  rustup run 1.97.1 cargo test  --locked --all-targets --all-features --no-fail-fast
  rustup run 1.97.1 cargo clippy --locked --all-targets --all-features -- -D warnings
'

# focused Hosted V2
# cargo test --locked --test hosted_remote_v2
# cargo test --locked --test hosted_remote_v2_recovery
# cargo test --locked --test hosted_remote_cli
# plus the exact durable_v2_integrated_dev_submit_and_checkpoint_recovery test

# fuzz (nightly, separate)
# cargo +nightly fuzz run parser
```

Term kit `o test` is `cargo test --all-targets` **in `$O_LANG_ROOT`**. That is the live tree. I would not use it while you are developing here.

---

## 16. One non-language script

### `big_iron_to_my_texas_red.sh` (also under `scripts/`)

Creates or replaces a GitHub repo with `gh`. Default **private**. `--overwrite` can delete a remote. `--solo` strips collaborators.

```bash
./big_iron_to_my_texas_red.sh --help
./big_iron_to_my_texas_red.sh --public my-repo
```

Not part of the language toolchain.

---

## 17. Everyday “I want to do X” cheat sheet

| Action | Command |
|---|---|
| First install | `./setup.sh -y --minimal --verify` then `source ~/.config/ostadix/env.sh` |
| Update tools | `o pull` |
| Health | term-kit `o doctor`; MCP `o_doctor`; `o kernel doctor` |
| Run a program | `O file.O "$O_LANG_ROOT/backends"` or `o run file.O` |
| One-shot expr | `O --eval '…'` or term-kit `o e '…'` |
| REPL | `O --repl backends` |
| See the plan | `olangc file.O --target ir --shim-dir backends` |
| Why P3 | `o why file.O P3` |
| Graph | `olangc file.O --target dot --shim-dir backends \| dot -Tpng -o g.png` |
| AOT | `olangc file.O -o out --shim-dir backends` |
| Full ship | term-kit `o ship file.O` |
| Safe link | `o-link --project . -o app.O` |
| Literal link+run | `o-link dir/` (dangerous) |
| Unlink | `o-unlink app.O -o restored/` |
| Live demo | `o-live-host demo --state DIR` or `o live demo` |
| Live official gate | `bash scripts/smoke-hosted-live-reference.sh` |
| Node V2 walkthrough | README Hosted V2 block (`o-node serve` + `octl node authority dev-mint` + session) |
| Kernel build | `o kernel build` |
| Kernel interactive | `o kernel boot` (`Ctrl-A X`) |
| All portable QEMU gates | `./boot-and-test.sh smoke` or `o kernel gates` |
| Example suite | `bash test_o_lang_examples.sh` |
| CLI contract | `bash tests/test_cli.sh` |
| Claims / evidence | `bash scripts/check_release_claims.sh` |
| Source ZIP | `python3 scripts/build_source_release.py --output dest.zip` |
| MCP smoke | `python3 scripts/smoke_ostadix_mcp.py` |
| Docker smoke | `bash scripts/smoke-docker.sh` |
| Rust gate | Multipass `moral-gaur`, not this checkout |

---

## What I could not verify this turn

- I did not execute hello.O, setup, any smoke, any QEMU gate, or any cargo test.
- `o-info` is missing from `target/release` on this tree. I stated that from `ls`.
- I did not walk every `ocore/kernel/smoke-*.sh` body line-by-line; the probe map is from `boot-and-test.sh` plus each script’s name and the CLAIMS/README table.
- Term-kit extras (`o doctor`, `o dash`, SSH, showreel) live in `~/.config/ostadix/term/`, not in the repo’s `scripts/`. They are real on this machine; they are not the repository dispatcher.

If you want the next pass to be “run every hosted smoke and report the actual output,” say so and I will do it without touching the live Cargo tree.

## User

why --minimal

## Assistant

`--minimal` is the hosted-core profile. I’ll read exactly what it turns off versus default/`--full` so the reason isn’t a slogan.

## Tools

- Read: /Users/ustad/Ostadix-lang/setup.sh (139-258)
- Search: MINIMAL|FULL|WITH_NIX|WITH_OCORE|WITH_HOSTED|notebook|prompt|if \$MINIMAL|if \$FULL
- Read: /Users/ustad/Ostadix-lang/setup.sh (681-770)
- Read: /Users/ustad/Ostadix-lang/setup.sh (1121-1250)
- Read: /Users/ustad/Ostadix-lang/setup.sh (381-470)
- Search: MINIMAL|prompt|ask_|read -
- Search: YES|ask|confirm|extras|Nix\?|install Nix|read -r
- Read: /Users/ustad/Ostadix-lang/setup.sh (1261-1460)
- Read: /Users/ustad/Ostadix-lang/setup.sh (914-938)
- Search: --minimal|--full|minimal
- Search: --minimal|minimal hosted|hosted profile
- Read: /Users/ustad/Ostadix-lang/README.md (701-760)

## Assistant

`--minimal` is the hosted-core profile. `o pull` and `o build` use it so a tool refresh rebuilds `O` / `olangc` / shims / C17 / MCP and does **not** install Nix, QEMU, Racket, WASI, or the notebook.

That is the contrast that matters. `--minimal` is not “a weaker language.” It is “do not change the rest of the machine.”

`--full` is a different host shape. The script turns those extras on itself:

- `WITH_OCORE=true` — Clang, LLD, ELF tools, CMake, `qemu-system-x86_64` and `qemu-system-aarch64`
- `WITH_NIX=true` unless you pass `--no-nix` — official Nix installer (daemon path on macOS/systemd Linux)
- `o-notebook` plus `cargo build --features notebook`
- Racket on the package maps
- `rustup component add rustfmt clippy` and `rustup target add wasm32-wasip1`

`--with-hosted-runtimes`, `--with-ocore-media`, `--with-guest-tools`, `--with-ubuntu-vm`, and `--with-linux-kernel-tools` are even heavier and stay off unless you name them. Setup never downloads a guest OS; `--full` still pulls privileged or large host packages. A daily rebuild of the evaluator does not need any of that.

`--minimal` and `--full` are mutually exclusive so you cannot compose them by accident. `--minimal` still accepts explicit `--with-*` if you want one extra (O-core tools, Nix, …) without taking the whole `--full` set.

Against a bare `./setup.sh` with no profile flag, the **build is the same** right now: same Rust bins (`O`, `olangc`, `ocorec`, `o-link`, `o-unlink`, `ogit`, `o-live-host`, `o-node`, `octl`, `o-registry`, `o-info`), same C17 edition, same MCP, same wrappers. The remaining `--minimal`-only behavior in the current script is skipping the optional matplotlib check (`computed_plot.O`). The README says that. The header comment about “no prompts for nix/extras” is leftover: extras are now flags, not a prompt I found in this `setup.sh`.

So agents and the term kit say `--minimal` because:

1. Refreshing tools should match the commit, not re-provision the lab.
2. Nix/QEMU/Racket/WASI/notebook are optional scopes with their own claims and deps.
3. `-y --minimal` is non-interactive and will not grow the host.
4. You can still add one `--with-*` when you actually want that substrate.

Use `--full` when you want notebook + Nix + O-core/QEMU on this machine. Use `--with-ocore --verify-ocore` when the thing you want is the bounded kernel smoke, not a bigger hosted profile.

## User

how can i get all

## Assistant

`--full` is not “all.” I’ll confirm which flags actually stack so you get one command that covers every profile this Mac can take.

## Tools

- Read: /Users/ustad/Ostadix-lang/setup.sh (60-139)
- Read: /Users/ustad/Ostadix-lang/README.md (721-770)

## Assistant

`--full` is not all. On this Mac the widest `setup.sh` will accept is:

```bash
cd /Users/ustad/Ostadix-lang

./setup.sh --full \
  --with-hosted-runtimes \
  --with-ocore-media \
  --with-guest-tools \
  --with-ubuntu-vm \
  --verify \
  --verify-ocore \
  -y \
  --persist-env \
  --dry-run
```

If that plan looks right, drop `--dry-run` and run it for real.

`--full` already turns on Nix, O-core/QEMU tools, Racket, WASI (`wasm32-wasip1`), rustfmt/clippy, and `o-notebook`. The extra flags add what `--full` deliberately leaves off:

| Flag | What you get |
|---|---|
| `--with-hosted-runtimes` | Node, Ruby, Racket, GHC, OCaml, SBCL, Mono, Octave (bottle only), WABT, Wasmtime |
| `--with-ocore-media` | GRUB EFI, mtools, OVMF — so `o kernel media` / `smoke-media` can work |
| `--with-guest-tools` | `qemu-img`, xz, zstd, guest-lab dir (no OS image is downloaded) |
| `--with-ubuntu-vm` | Multipass for `ubuntu_vm^` |
| `--verify` | hosted Rust / C17 / AOT / Python hello |
| `--verify-ocore` | bounded x86 QEMU kernel smoke |
| `-y` | non-interactive for the selected profiles (Nix installer included) |
| `--persist-env` | source `~/.config/ostadix/env.sh` from your shell rc |

`o pull --full` is only `./setup.sh --full -y`. It will not install the hosted-runtime pack, UEFI media tools, guest lab, or Multipass.

Do **not** add `--with-linux-kernel-tools` here. That flag is Linux-only; on macOS `setup.sh` exits. Use it inside Multipass `moral-gaur` if you want that profile:

```bash
multipass exec moral-gaur -- bash -lc \
  'cd /home/ubuntu/Ostadix-lang && ./setup.sh -y --full --with-hosted-runtimes --with-ocore-media --with-linux-kernel-tools --verify --verify-ocore'
```

What you still will not get, by design:

- Java (local no-JRE policy)
- licensed MATLAB / Wolfram (Octave covers MATLAB-compatible code only)
- a guest Linux/9front/OpenBSD image (you supply checksum-pinned media)
- a Linux source tree (the kernel-tools profile is host packages only)
- Mode 21 AMD/KVM execution (needs nested SVM and `/dev/kvm`)

After a real run:

```bash
source ~/.config/ostadix/env.sh
O examples/hello.O "$PWD/backends"    # expect [number] 2
O version --json
./setup.sh --full --with-hosted-runtimes --with-ocore-media --with-guest-tools --with-ubuntu-vm --check
```

I have not run this. It will install Nix (daemon), QEMU, a large Homebrew set, and Multipass, and it will `cargo build --release` in this tree. Say if you want me to launch it.