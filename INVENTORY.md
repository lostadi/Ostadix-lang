# Ostadix-lang — Repository Inventory

**Compiled:** 2026-08-28 (local, Mac/ARM) by running the actual toolchain, not by reading docs.
**Scope:** Everything that exists in `/Users/ustad/Ostadix-lang` that you have built and can run.
**Method:** `o_env` / `o_doctor` / `o_smoke` via the Ostadix MCP, the `test_o_lang_examples.sh`
suite, direct binary probes, `ocorec`, `olangc` AOT (Rust + C editions), and the legacy Python
evaluator. No changes were made to the language tree during this inventory (new file only; a
throwaway `/tmp` AOT harness was used for build checks and cleaned up conceptually — artifacts
left in `/var/folders/.../opencode/` if you want to inspect them).

---

## 1. One-screen state

| Fact | Value |
|---|---|
| Canonical language | **O-lang / Ostadix-lang** (one language, three editions) |
| Version | **0.4.0** (`O 0.4.0` reported live) · git tag history: v0.2.0, v0.3.0 |
| Commits | 589 on `master`, last commit 2026-08-27 |
| Remote | `https://github.com/lostadi/Ostadix-lang.git` |
| Rust workspace | root crate `o-lang` + `crates/ostadix-api` (v0.4.0, independent engine) · 197 `.rs` files |
| Backends | **30 total**: 7 builtin (`O`, quote, nix/nix_expr/nix_store, html, markdown, latex, text) + 22 located shims + 1 missing (`mathematica`→`wolframscript` not installed) |
| Smoke | `O examples/hello.O backends` → **`[number] 2`**, exit 0 (MCP `o_smoke` = SMOKE_OK) |
| Example suite | **39 passed / 0 failed / 5 skipped** (opt-in: 2×nixos_test, 2×group_pipeline, 1×manual plan9_browser) |
| O-core `.oc` sources | **132** files (118 x86_64 runtime + world + aarch64 g2) · `ocorec` compiles `minimal.oc` → `minimal.mir`, exit 0 |
| Built binaries | 12 of 14 bins in `target/release/` (missing: `o-notebook` — `notebook` feature off; `ocore-kernel-world-record` — not in default build) |
| Alternate editions | **C17 `c_cpp/` edition: clean `make` succeeds, AOT `olangc` emits and runs native `hello` → `2`**. **Legacy Python `o_lang/`: runs `hello.O` → `2`**. |
| WASM AOT | `olangc --target wasm` — *not currently runnable on this Mac*: `rustup target wasm32-wasip1` not installed. Native AOT (default `--target binary`) works and runs. |
| Artifacts on disk | UEFI boot ISO (`target/ostadix-iso/.../ostadix-x86_64-uefi.iso`, OVMF-booted, evidence in commit `7e6ef401`), foreign-kernel lab (Alpine/FreeBSD/Guix/9front/Redox boot timings), `kernel.elf` (2.2 MB static x86-64 ELF), capacity ISO dir. |
| Whitepaper | `Ostadix-lang_Technical_Whitepaper.pdf` (1.5 MB, + `.tex` in `docs/`); `llms.txt` agent briefing; `docs/` = 30 design/spec docs. |
| Tests | 46 top-level integration `.rs` files in `tests/` + 28 `.py` (fixtures/support), 100 files in `src/`+`crates/` with `#[test]`s. |
| CI | `.github/workflows/ci.yml` + `fuzz.yml` (parser fuzz campaign). |

---

## 2. Binaries you have built (`target/release/`)

14 bins are declared in `Cargo.toml`. 12 are built and running right now:

| Bin | Status | Purpose (verified via `--help` / run) |
|---|---|---|
| `O` (2.8 MB) | ✅ runs, exit 0 | Hosted interpreter. `O file.O backends`, `--json`, `--check`, `--eval`, `--repl`, `O 0.4.0` |
| `o-cli` (6.3 MB) | ✅ built (backed by `o run/plan/ship/doctor/...` wrappers, not on PATH as-is; `~/.local/bin/o` re-exports it via `scripts/o-cli.sh`) | CLI façade per `ostadix-term` skill |
| `olangc` (7.7 MB) | ✅ runs | AOT: `--target binary` (default) / `wasm` / `script` / `ir` / `dot`. Embedded 23 shim scripts. **Native AOT of `examples/hello.O` built in 56 s and printed `2`** |
| `ocorec` (0.9 MB) | ✅ runs | O-core freestanding compiler: `--emit ast\|hir\|mir\|asm\|obj`, `--target x86_64-unknown-none`. Verified: `ocorec ocore/examples/minimal.oc --emit mir` → `minimal.mir`, exit 0 |
| `o-link` (5.7 MB) | ✅ built | Link files/dirs into one `.O`; `--literal` / `--project`; link-and-run by default |
| `o-unlink` (0.8 MB) | ✅ built | Restore a lifted tree |
| `o-notebook` | ⬜ not built (opt-in `notebook` feature; would need `--features notebook`) | Notebook host |
| `ogit` (0.6 MB) | ✅ built | O-Git semantic ledger: `demo`, `diff-semantic`; live receipt at `.ogit/receipts/semantic-receipt-001.json` |
| `o-live-host` (2.1 MB) | ✅ runs, `--help` clean | Live-World CAS: `pack/install/activate/upgrade/invoke/compose/rollback |
| `o-node` (6.0 MB) | ✅ built | Zero-config LAN node: `start/stop/status/restart/pair/pki/identity` |
| `o-registry` (1.2 MB) | ✅ built | Signed placement-registry snapshots: `init/profile-local/publish-profile/verify/list` |
| `o-info` (1.1 MB) | ✅ built | Authority-free Information store: `init/keygen/record/verify/import/head` (matches the `o_information_inspect` MCP tool) |
| `octl` (4.6 MB) | ✅ built | Control CLI for an admitted hosted node: `octl node inspect/invoke` |
| `ocore-kernel-world-record` | ⬜ not in current build (not in default feature set / not in release dir listing) | Kernel-world record helper |

MCP: `~/.local/bin/ostadix-mcp` (the `olang` MCP server used at the top of this session) =
`mcp/ostadix_lang_mcp_server` (own Cargo.lock, not in workspace). Registered in `.mcp.json`.
Connected `o`/`ostadix` wrappers also in `~/.local/bin`.

## 3. O-language surface (what runs today)

**Syntax** — `LANG^(…)_LANG` with optional `[n]` persistent env (`python[0]^(...)_python[0]`);
registered-alias support (`py`, `md`, `tex`, `plain`, `o`); `$IDENT` binds to O values, `\$var`
splices host env; `quote^(…)_quote` for unevaluated O source.

**30 backends** (from the live `o_doctor` catalog above): builtin = `O,quote,nix_expr,html,
markdown,latex,text`; located shims (22 `.py` files in `backends/`) = `bash,shell,python,
javascript,ruby,rust,c,cpp,java(java+java),nix,nix_store,nixos_test,sql,haskell,ocaml,racket,
lisp,common_lisp,csharp,matlab(octave),mathematica(missing→wolframscript),webassembly
(wat2wasm+wasmtime),ubuntu_vm(python3+multipass)`.

**What I ran, in this session, that actually worked:**
- `examples/hello.O` via MCP `o_smoke` → `[number] 2`
- Full `test_o_lang_examples.sh` → **39 pass / 0 fail / 5 skip** (see above for skips)
- Rust AOT: `olangc examples/hello.O -o /tmp/... --shim-dir backends` → native binary → `2` (exit 0)
- C17 AOT: `c_cpp/./olangc ../examples/hello.O -o /tmp/...` → native binary → `2` (exit 0)
- Legacy Python: `python3 -m o_lang examples/hello.O` → `2`
- `ocorec ocore/examples/minimal.oc --emit mir` → `minimal.mir`, exit 0
- `octl`/`o-info`/`o-registry`/`o-node`/`o-live-host`/`ogit`/`o-link`/`o-unlink` — `--help` clean

**Skipped, not failures:** 2× `nixos_test` (opt-in `RUN_NIXOS_TESTS=1`), 2× group_pipeline
(opt-in `RUN_GROUP_PIPELINE_EXAMPLES=1`), 1× `plan9_browser` (manual).

---

## 4. O-core (`.oc` freestanding systems language)

- **132 `.oc` files** under `ocore/`. Key clusters:
  - `ocore/runtime/x86_64/` — 70+ modules (world, kernel-world boot/record/execution, m5 REPL/
    supervisor/init/selftest, m6 Linux personality/supervisor/observer, m6b live variants,
    m7 Plan 9 client + m7b logical-read, ipc, elf_loader, image_vfs, memory/vm objects, scheduler,
    svm_execution, thread/domain/namespace, personality, capability transfer, delegated resource,
    endpoints, native control/ABI, m1/m3 live stubs, smp probe, …)
  - `ocore/runtime/aarch64/` — `g2_kernel.oc`, `g2_user_a.oc`, `g2_user_b.oc` (G2 kernel, aarch64)
  - `ocore/kernel/` — m1, m2, m3, m3_live, m4, m5, m6, m6b, m7b, world_identity/value/receipt/
    protocol, kernel_world_semantics, kernel_world_execution_device_semantics, linux_personality*
    (semantics + stubs), `main.oc`, `scheduler_bridge.oc`, `linker.ld`, `boot.S`, plus **30
    QEMU smoke scripts** (`smoke-*.sh`: faults, ipc, ipc-foundation, kernel-world execution/live/
    device, loader, live bounded persona, personas, processes, scheduler, smp, UEFI ISO/media,
    world identity/protocol/receipt/runtime/project receipts, x86_64 boot-info, world value,
    stress-live-linux-personality, etc.) and 8 `build-*-artifacts.sh` scripts (m4, m5, m6, m6b
    live, m7 linux/plan9, m7b logical, **x86_64 UEFI ISO** `build-x86_64-uefi-iso.sh`, and capacity
    ISO).
  - `ocore/user/` — linux-minimal guest corpus tooling (`OVFS_IMAGE_V1.md`, `make_m4_elf_corpus.py`,
    `pack_ovfs.py`, `verify_ovfs.py`, `linux-minimal-oracle.json`, 9p2000 / m7b / linux-minimal
    test scripts, `static-user.ld`, `linux-minimal-user.ld`)
  - `ocore/world/` — `codec.oc,identity.oc,protocol.oc,receipt_codec.oc,receipt.oc,sha256.oc,
    value_codec.oc,value.oc` (the world value/protocol/receipt type system)
  - `ocore/examples/minimal.oc` (verified compilable via `ocorec --emit mir`)
- **`ocorec`** is the compiler (built, runs, emits AST/HIR/MIR/asm/obj for x86_64-unknown-none).
- **`kernel.elf`** at repo root: 2.2 MB statically-linked x86-64 ELF (dated 2026-07-28 in this
  checkout — a previously-built kernel image).
- **Boot ISO, OVMF-booted, with foreign-kernel boots** — evidenced in commit `7e6ef401`:
  - ISO: `target/ostadix-iso/.../ostadix-x86_64-uefi.iso`, 9,734,144 bytes,
    SHA-256 `d9578053a24237c55cbd86a2e608d834c460bd96954fea60510e655076cb0e18`
  - Boots verified: Alpine 1.73 s, FreeBSD 39.74 s, 9front Plan 9 19.20 s, Guix System 47.87 s,
    Redox 7.17 s (per that commit's message; lab tree at `target/foreign-kernel-lab-current/`)
- **QEMU** present on this machine (`qemu-system-x86_64`, `qemu-system-aarch64`) — all 30 smoke
  scripts are runnable when you choose to run them (not run in this pass; that's a longer op).
- **`okernel-multikernel/`** and **`plan9/`** top-level dirs: present but `plan9/` was empty in
  this checkout's listing; treat `okernel-multikernel/` as the kernel-mesh experiment area
  (see `06-gaps-notes.md`).

## 5. What it is, top-level, per directory (the rest)

- `src/` + `crates/ostadix-api/` — Rust workspace. 197 `.rs` files, ~100 containing unit tests.
  Root crate exposes all 14 bins. `ostadix-api` = the "independent runtime engine" (v0.4.0,
  default feature `graph_executor`; see commit `e74cda5a` "make ostadix-api the independent
  runtime engine" and `e20fe0f4` "freeze M2 execution fabric loopback").
- `backends/` — 22 `*_shim.py` files + `manifest.json`, etc (the shim set for `O` AOT execution).
- `examples/` — 52 top-level entries; 42 are `.O` programs + fixtures (`computed_plot.html`,
  `hello.html`, `t.html`, `manifest.json`, `literate_report.md`, `group_pipeline/`,
  `docker_literal/`, `plan9/`, `plan9_test.html`, `plan9.html`) + a bunch of single-topic demos
  (bash/js/shell/sql/nix/nixos/html/markdown/latex/quote/meta_eval/persist/lazy/instantiate/
  realise/os_as_participant/nested_splice/coordination_groups/env_split/ephemeral/semantic_
  custody/trailing_expr/bindings/script/computed_plot/literate_report/plan9_browser/nixos_test/
  nixos_test_two_machine/nix_storepath[_python]/nix_basic/nix_python_html).
- `tests/` — 46 top-level integration `.rs` + 28 `.py` (fixtures/support). Covers: autonomous
  hosted parallel, backend morphism v1, execution fabric two-node, execution intent CLI,
  executor actors/effects/reentrancy/state-complete, hgraph ontology + schedule + proptest,
  hosted live cli/reference/supervisor transactions + remote v1/v2/recovery, independent
  runtime engine, information bridge v1, kernel world contract, o-cli intent blackbox, o-info
  cli, olink project + proptest, parser proptest, placement v6, project deployment plan, project
  hgraph exec + logical + integration, project mesh cli, project world runtime, registry v1,
  runtime adapter closure, runtime exec capacity/TOCTOU, semantic custody demo, fixtures/support.
- `docs/` — 30 design/spec docs + `releases/{v0.2.0,v0.3.0}.md` + `Ostadix-lang_Technical_
  Whitepaper.tex`. See the full list in the directory. Notable: ARCHITECTURE.md, SPEC.md,
  OIR_EXECUTION_FABRIC_V1.md, HGRAPH_EXECUTOR_PLAN.md, HOSTED_PLACEMENT_V6.md,
  KERNEL_WORLD_CONTRACT.md, PROJECT_MESH_V1.md, INFORMATION_KERNEL_V1.md,
  M3_AUTHENTICATED_PURE_REMOTE_EXECUTION_DESIGN.md, SEMANTIC_CUSTODY.md,
  RELEASE_CHECKLIST.md, VERSIONING.md, CI_POSTURE.md, ABSORBED_CAPACITY.md, FOREIGN_KERNEL_LAB.md,
  IMAGE_ADMISSION.md, O_MACHINE_CONTRACT.md, OSTADIX_BOOT.md, OSTADIX_WORLD.md, WORLD/PERSONALITY
  memory, ZERO_CONFIG_{LAN,INSTALLER_V2,E0382_HOTFIX,VERIFICATION}.md.
- `evidence/` — TOML evidence files for gate checks: `absorbed_capacity_catalog.toml`,
  `absorbed_capacity_iso.toml`, `foreign_kernel_lab.toml`, `gates.toml`,
  `o_machine_contract_v1.toml`, `world_contract_v1.toml`, `world_contract_v2.toml`,
  `world_alpha_gates.toml`, plus `evidence/world/` (15 G0-era evidence+supersession TOMLs
  dated through 2026-08-17). These are the "independently inspectable" claims files the docs
  keep referencing.
- `ci/` — `architecture-roots.toml`, `required-jobs.toml`, `test-suites.toml` (CI gate manifests
  used by `.github/workflows/ci.yml`).
- `setup/` + `setup.sh` — platform setup (12 platform scripts in `setup/os/`: alpine, arch,
  debian, fedora, freebsd, gentoo, macos, nixos, opensuse, tinycore, void, windows).
- `mcp/ostadix_lang_mcp_server` — the Ostadix MCP server (Rust, `rmcp`, tokio full; not a
  workspace member — own lockfile). Used at the top of this session (`o_env`/`o_doctor`/
  `o_smoke`/`o_run`/`o_olangc`/`o_information_inspect`/`o_analyze_intent`/`o_execute_intent`).
- `tools/lsp/ostadix-lsp` — LSP server (Python, own `venv/`).
- `fuzz/` — parser proptest/fuzz corpus (`corpus/`, `fuzz_targets/`, own lock) — used by
  `fuzz.yml`.
- `media/`, `assets/`, `Olang_Mascot_little-o/little-o/` — logo + "little-o" mascot assets.
- `Ostadix-lang_Technical_Whitepaper.pdf` (1.5 MB) + `ostadix-demo-assets/` — whitepaper (also a
  `.tex` in `docs/`) + demo assets.
- `.ogit/receipts/` — one live receipt (`semantic-receipt-001.json`) from `ogit demo`.
- `.modloop/`, `.ocore-repair-backups/`, `.remember/`, `.pytest_cache/`, `.opencode/` — local
  scratch/bookkeeping, not part of the ship.
- `big_iron_to_my_texas_red.sh` (+ `.1`) — big-iron→Texas-red migration helper (script pair at
  root; a copy in `scripts/`).
- `o_lang/` — **Legacy Python edition** of O (parser, evaluator, ovalue, cli, backends). Status:
  "Reference Only" per its README. Verified: `python3 -m o_lang examples/hello.O` → `2`.
- `c_cpp/` — **C17 edition**. Standalone `cc`-based build (value.c, parser.c, process.c, eval.c,
  scheduler.c, nix_ops.c, nixos_ops.c, main.c, olangc.c, CMakeLists + Makefile + `tests/`,
  `include/`). Verified: clean `make` succeeds; its `./olangc ../examples/hello.O` emits a
  running native binary that prints `2`.
- `Ostadix-lang/` (nested dir at repo root) — an **older snapshot of the *same* repo** (own
  `.git/`, 716 files non-target, remote = same GitHub repo, last commit there: `3742190a`
  "Merge pull request #5 from lostadi/feature/semantic-unification-hosted-v2"). Its README
  differs from the top-level README. **Not** on the current `master` build path per the
  toolchain above — treat it as a checkout you happened to leave at the root, not a dependency.
  (See `06-gaps-notes.md` for a recommendation.)
- `plan9/` — present, **empty** at this moment.
- `benchmarks/hgraph_hosted/` — HGraph hosted benchmarks, driven by
  `scripts/benchmark_hgraph_hosted.sh`.
- `dev/`, `build/`, `target/`, `src/`, `sys/`, `proc/`, `root/`, `run/`, `srv/`, `mnt/`, `opt/`,
  `home/` — the unusual `sys/proc/root/run/srv/mnt/opt/home` dirs at the repo root look like a
  Linux-style rootfs tree someone (or some tool, possibly a Plan 9 / O-core kernel build or an
  `o-live-host` fixture) dropped at the root. Only `run/lock` was populated in the listing I
  took; the others were empty. Flagged in `06-gaps-notes.md`.
- `newroot/`, `boot-and-test.sh`, `kernelsu_patched_20260820_052312.img` — kernel/phone-patching
  work (Kernelsu-patched image dated 2026-08-20).
- `google_images.html` — a stray HTML file at root (search-result dump, not code).
- `session-ses_fe33.md`, `ostadix-lang-info.md`, `ORIGIN.md`, `llms.txt`, `olango-c-backend.patch`
  (`olang-c-backend.patch`), `Olang_Mascot_little-o/`, `opencode.jsonc` (OpenCode config for this
  repo), `opencode.jsonc` — agent/project config/prose.
- `Dockerfile`, `.dockerignore`, `smoke-docker.sh` — Docker packaging for the runtime.
- `NOTICE`, `LICENSE` (LGPL-2.1-only), `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`,
  `CITATION.cff` — governance/citation files.
- `CHANGELOG.md` — version history.
- `AGENTS.md` — the agent instruction file that is injected into every session on this repo
  (the one at the top of this conversation).
- `rust-toolchain.toml` — pins the Rust toolchain for the workspace.

---

## 6. How to re-verify this inventory in one shot

```bash
cd /Users/ustad/Ostadix-lang
export O_LANG_ROOT=$PWD O_BACKENDS_DIR=$PWD/backends \
       PATH="$HOME/.local/bin:$PWD/target/release:$PATH"

# 1. Smoke (expect `[number] 2`)
O examples/hello.O backends

# 2. Full example suite (expect 39 pass / 0 fail / 5 skip)
bash test_o_lang_examples.sh

# 3. Native AOT (Rust runtime, expect `2`)
olangc examples/hello.O -o /tmp/oo --shim-dir backends && /tmp/oo

# 4. Legacy Python AOT, expect `2`
python3 -m o_lang examples/hello.O

# 5. C17 AOT, expect `2`
cd c_cpp && make -s -j4 O olangc && \
    olangc ../examples/hello.O -o /tmp/oc && /tmp/oc; cd -

# 6. O-core MIR
ocorec ocore/examples/minimal.oc --emit mir
```

---

## 7. Gaps & notes (what I did *not* claim)

- `mathematica` backend = *missing* (`wolframscript` not on PATH). 29 of 30 backends are located.
- `o-notebook` is not in `target/release/` (needs `--features notebook`).
- `ocore-kernel-world-record` bin is declared in `Cargo.toml` but not in the current release
  dir — needs a rebuild to appear, or it's built by a different feature I didn't enable here.
  I did **not** claim it runs; I only listed it.
- `--target wasm` AOT fails on this Mac until `rustup target add wasm32-wasip1` is run.
- The 30 O-core QEMU smokes are *present* and QEMU is *installed*, but I did not re-run all of
  them this pass (each is a multi-second boot). I ran `ocorec` on one file instead. The most
  recent full OVMF boot set is the one captured in commit `7e6ef401` (ISO + Alpine/FreeBSD/
  Guix/9front/Redox timings).
- The nested `Ostadix-lang/` checkout and the rootfs-looking `sys/`, `proc/`, `root/`, etc.
  directories are real but not on the current `master` build path — treat as leftovers/scratch
  unless you say otherwise.
- `plan9/` at the root was empty in this snapshot.
- I did not run `cargo test` here (policy: don't `cargo` in the live tree while Lee is
  developing — use Multipass `moral-gaur`). If you want a full `cargo test --lib` + the G0
  conformance gates, say the word and I'll do it on the VM and paste the report.