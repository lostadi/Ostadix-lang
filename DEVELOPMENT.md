# Ostadix-lang Developer Guide

Practical notes for contributors to [Ostadix-lang](https://github.com/lostadi/Ostadix-lang), short name **O-lang**. The project is a polyglot language system where typed expressions choose their evaluator with `LANG^(...)_LANG`, and O-core is the separate freestanding native systems language.

## Implementations in this repository

There are three active, supported hosted `.O` implementations plus O-core:

- **Rust hosted implementation** (`src/`) is authoritative. It provides the `O` interpreter, OIR planner/evaluator, backend registry, scheduler, linker tools, notebook server, `olangc`, and `ocorec`.
- **C17 hosted implementation** (`c_cpp/`) is active and supported. `make` or CMake build the interpreter `O` and AOT compiler `olangc` from C sources.
- **Python hosted implementation** (`o_lang/`) is active and supported as a readable semantic reference and cross-check target.
- **O-core** (`src/ocore/`, `docs/OCORE.md`) is a freestanding native systems language. It compiles `.oc` source through AST, typed HIR, and SSA MIR to x86_64 ELF object files. That x86_64 ELF restriction applies only to O-core's emitted object format, not to hosted `.O` execution.

`c_cpp/legacy_cpp/` is the historical C++ prototype only. It used an obsolete line-oriented JSON protocol and is not built by the active C17 Makefile or CMake paths.

## Build and run

```bash
# Rust interpreter (Cargo default-run is O)
cargo run -- examples/hello.O
cargo run -- examples/hello.O backends

# Rust binaries
cargo build --all-targets --all-features
cargo run --bin olangc -- examples/hello.O -o build/hello_rust_aot
cargo run --bin ocorec -- --help

# Python reference CLI
python3 -m o_lang examples/hello.O
python3 -m o_lang examples/hello.O --dump-ast
python3 -m o_lang examples/hello.O --as json

# C17 implementation
make -C c_cpp
c_cpp/O examples/hello.O backends
```

## Rust architecture map

- `src/parser.rs` parses typed parentheses into `ONode` trees. An identifier is an opener only if the registry accepts that language tag or alias.
- `src/ir.rs` lowers syntax to OIR and contains `BackendSpec` / `BackendRegistry`, the single source of truth for accepted evaluator tags, aliases, cache-safety, splice renderers, execution mode, required authorities, and shim path resolution.
- `src/eval.rs` executes validated OIR plans. It projects OIR to the directed HGraph and uses the state-complete graph coordinator by default. `O_EXECUTOR=serial` selects `execute_plan_serial` as the semantic differential oracle.
- `src/effects.rs` defines shared effect confidence, fallibility, typed resources, checked source declarations, and conservative backend classification. Unknown hosted operations use `HostWorld`.
- `src/executor/` contains the readiness coordinator and the worker path for verified pure inline renderers. The coordinator owns evaluator-local mutable state; it never sends the process registry across worker threads.
- `src/value.rs` defines `OValue`, the shared value vocabulary crossing language boundaries.
- `src/wire.rs` implements the hosted evaluator protocol: **4-byte length-prefixed canonical CBOR** frames.
- `src/hgraph/` builds and validates the directed execution HGraph. OValue, completion, resource-state, and actor-state values are nodes; evaluator invocations are multi-output hyperedges. `ReadySchedule` derives blockers only from input producers.
- `src/scheduler.rs` schedules autonomous request forcing and cache interaction.
- `src/capability.rs` models runtime authority-bearing capabilities and snapshots.
- `src/process.rs` manages backend shim subprocess lifetimes and requests.
- `src/backend.rs` contains backend protocol types shared with shims.
- `src/shims.rs` supports embedded/extracted shim assets for compiled hosted programs.
- `src/nix_ops.rs` and `src/nixos_ops.rs` implement Nix and NixOS operations used by builtins.
- `src/ocore/` contains the O-core lexer, parser, AST, HIR, type checker, MIR, codegen, driver, and capability bridge.
- Binaries: `src/main.rs` is `O`; `src/bin/olangc.rs`, `ocorec.rs`, `olink.rs` (`o-link`), `ounlink.rs` (`o-unlink`), `ogit.rs`, and `o-notebook.rs` provide compiler, native, linker, Git, and notebook entry points.

## Environment semantics

Bare hosted blocks are ephemeral: `lang^(...)_lang` gets a fresh evaluator instance and it is discarded after the block. Persistent state is explicit only with environment-indexed tags such as `lang[0]^(...)_lang[0]` or `lang[7]^(...)_lang[7]`. The Rust evaluator represents ephemeral dispatch with `env_id == u32::MAX` and cleans that environment after execution; `SPEC.md` documents the same rule.

## Backend registry and shims

To add a Rust-hosted language backend:

1. Add a static `BackendSpec` entry to `BACKEND_SPECS` in `src/ir.rs`: canonical name, aliases, cache-safety flag, splice renderer, execution mode, and required authorities.
2. Add a shim under `backends/` when the backend is `ExecutionMode::Shim`. Shims speak the 4-byte length-prefixed canonical CBOR command/response protocol.
3. Rebuild. Registration is compile-time/static and registry-extensible; it is not a runtime plugin system.
4. Add tests or examples that exercise parsing, rendering, execution, environment lifetime, and any authority requirements.

Do not add per-binary backend lists or ad hoc evaluator dispatch tables. Binaries call `BackendRegistry::global().registered_backend_tags()`.

Cache-safe `{lazy}` backends are `html`, `markdown`, `latex`, and `text`; impure or host-effecting backends should use `{defer}` for explicit forcing. Nix-family evaluation is not cache-safe under generic lazy memoization.

Effect attributes are semantic constraints, not authority grants. `effects=unknown`
downgrades a trusted renderer, while `effects=pure` is accepted only when the
derived backend classification is already verified pure. `reads=`, `writes=`,
and `serial=host` add typed resource dependencies. Declarations do not prove a
complete arbitrary host footprint, so host declarations keep the `HostWorld`
umbrella unless a future verified analyzer supplies a precise model.
Governed World/namespace epochs, Governor positions, node/domain/process and
resource generations, object versions, descriptive capability identities,
task attempts, artifact publications, devices, and accelerators are
trusted-lowering vocabulary only. Source declarations cannot mint them. Device
and accelerator keys expand to the same canonical generic governed-resource
dependency.

## Test and validation commands

CI (`.github/workflows/ci.yml`) runs all of these paths:

```bash
# Rust authoritative runtime
cargo test --all-targets --all-features
cargo test --test parser_proptest
cargo test --lib ocore::driver::tests::ocore_object_is_byte_reproducible_across_source_directories -- --exact
cargo check --manifest-path fuzz/Cargo.toml
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings

# O-core executable milestone evidence (requires Clang, LLD, Python, and QEMU x86_64)
python3 scripts/release_evidence.py validate
cargo test --test world_identity_wire
./ocore/kernel/smoke-world-identity-qemu.sh
cargo test --test world_receipt
./ocore/kernel/smoke-world-receipt-qemu.sh
./scripts/smoke-world-resource-keys.sh
./scripts/smoke-project-hgraph.sh
./scripts/smoke-project-hgraph-exec.sh
./boot-and-test.sh smoke

# Python reference runtime
python3 -m tests.test_parser
python3 tests/example_manifest.py validate
python3 -m tests.test_evaluator
python3 -m compileall -q o_lang backends tests

# C17 interpreter and AOT compiler
make -C c_cpp test
make -C c_cpp olangc-test
make -C c_cpp warnings-as-errors
cmake -S c_cpp -B build/c_cpp-cmake
cmake --build build/c_cpp-cmake
ctest --test-dir build/c_cpp-cmake

# Documentation/release-claim guard
bash scripts/check_release_claims.sh
python3 -m unittest -v tests.test_source_release
```

`smoke-project-hgraph.sh` is the composite hosted PR7 planning and generated
project-adapter gate. Its planning phase uses a real fixture to prove exact
bundle/policy provenance, all five project operation kinds, logical
alternative/prerequisite topology, stable nonexecuting IR/DOT output,
malformed/substitution rejection, conservative `HostWorld`, and ordinary `.O`
IR compatibility. It also proves `scripts/o-cli.sh plan` is byte-identical to
the direct `olangc --target ir` result. After those nonexecution checks, the
smoke compiles a project binary and runs bounded opt-in AnySuccess cases for
immediate short-circuit and nonzero-to-success continuation in disposable
workspaces. `setup.sh` installs lowercase `o` as a wrapper over the dispatcher
while preserving evaluator fallback for non-subcommand input.

`smoke-project-hgraph-exec.sh` is the separate PR8A/PR8B ordered hosted
execution gate. It proves that the opt-in HGraph coordinator owns isolated
materialization, typed prerequisite ordering, route settlement, and unsigned
lifecycle tracing for one `Explicit`/`Default` alternative plus serial
`Fallback`/`AnySuccess`. The latter use a first-class ordered-prefix input
policy, retain each attempted result, continue after nonzero or guard skip, and
never start a later branch after the first success. Infrastructure abort stops
without publishing a route result. This is not parallel race/cancellation,
retry, placement, deployment, Governor/receipt integration, exactly-once
effects, native or QEMU evidence, hardware isolation, G1, or G0--G13 passage.
Keep the installed-wrapper directories before `target/release` in `PATH`; on a
case-insensitive host the raw `O` release binary is otherwise also found as
lowercase `o` and shadows the dispatcher.

<!-- BEGIN GENERATED: REQUIRED_QEMU_EVIDENCE_DEVELOPMENT -->
The aggregate executes all 22 required portable QEMU gates in the
order declared by `evidence/gates.toml`, streams their output, and requires
every declared marker exactly once in each captured live transcript. The
manifest also records each gate's milestone, tools, evidence class, positive
claims, and explicit non-claims. `kernel-world-mode21-svm-kvm` is validated as
supplemental hardware evidence rather than part of this portable release set.
<!-- END GENERATED: REQUIRED_QEMU_EVIDENCE_DEVELOPMENT -->

The byte-reproducible O-core object test protects deterministic native object
output across source directories. `cargo test --test parser_proptest` runs parser
proptest properties, and `cargo check --manifest-path fuzz/Cargo.toml` ensures
the parser fuzz target still compiles.
The source-release tests protect the committed-ref allowlist, deterministic ZIP
bytes and canonical metadata, dirty-worktree boundary, symlink denial, relative
documentation-link closure, inert example/evidence/MCP schema and reference
validation, required release surfaces, and tamper detection.

## Conventions

- Keep the Rust implementation authoritative for hosted `.O` semantics; keep Python readable and C17 active rather than treating either as abandoned.
- Keep wire-protocol claims precise: hosted shims use 4-byte length-prefixed canonical CBOR.
- Keep `OWIDENT` v1 claims precise: it is the bounded cross-language identity
  oracle only, and serialized capability IDs are descriptive non-authority.
- Keep `OWPROTO` v1 claims precise: PR 3 provides deterministic bounded records
  and an offline negotiation function, not a transport, live handshake,
  authenticated authority path, OValue/extension envelope, or receipt codec.
  Mode 29's `OWVALUE` is a separate self-framed format, not a fifth protocol-v1
  kind.
- Keep `OWVALUE` v1 claims precise: PR 4 freezes only the bounded portable
  allowlist and root-only inert extension envelope, with canonical full-record
  SHA-256. It does not make the full hosted `OValue` portable, change hosted
  canonical-CBOR shims, create authority or transport, satisfy Workstream A, or
  implement the PR 5 receipt.
- Keep `OWRECEIPT` v1 claims precise: PR 5 is a bounded offline canonical
  receipt/signing-preimage oracle. Hosted Rust performs Ed25519 sign/verify with
  a pinned public conformance key; native Mode 30 validates only receipt and
  signature-envelope structure. Neither path supplies production key custody,
  trusted signer policy, live receipt emission, authoritative fencing, a World
  Alpha attestation, Acceptance A, or G0--G13 passage.
- Keep hosted ResourceKey PR6 claims precise: its smoke proves typed planner
  vocabulary, underlying identity helpers' caller-pair comparison, HGraph state
  chaining, alias-aware grounding partitioning, source-forgery rejection, and
  residual `HostWorld`. Grounding only checks the bound World epoch/membership.
  This is not Mode 31, a wire ABI, production governed lowering, native/QEMU
  evidence, Governor authority, device assignment, DMA/IOMMU isolation,
  Acceptance A, or G0--G13 passage.
- Keep HGraph wording precise: graph dispatch is implemented, but worker-pool execution is limited to verified pure inline renderers. Unknown shims are serialized through `HostWorld`, and exact arbitrary-source filesystem/network inference is not implemented.
- Prefer registry metadata over duplicated backend-name, purity, renderer, or authority tables.
