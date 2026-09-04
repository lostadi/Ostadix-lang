# Ostadix-lang Developer Guide

Practical notes for contributors to [Ostadix-lang](https://github.com/lostadi/Ostadix-lang), short name **O-lang**. The project is a polyglot language system where typed expressions choose their evaluator with `LANG^(...)_LANG`, and O-core is the separate freestanding native systems language.

## Implementations in this repository

There are three active, supported hosted `.O` implementations plus O-core:

- **Rust hosted engine** (`crates/ostadix-api/`) is authoritative for the
  parser, OIR planner/evaluator, backend registry, scheduler, runtime assets,
  and advanced APIs. The root `src/` package is its compatibility/CLI shell and
  provides `O`, the compiled `o-cli` intent orchestrator, linker tools,
  notebook server, `olangc`, and `ocorec`.
- **C17 hosted implementation** (`c_cpp/`) is active and supported. `make` or CMake build the interpreter `O` and AOT compiler `olangc` from C sources.
- **Python hosted implementation** (`o_lang/`) is active and supported as a readable semantic reference and cross-check target.
- **O-core** (`crates/ostadix-api/src/ocore/`, `docs/OCORE.md`) is a freestanding native systems
  language. It compiles `.oc` source through AST, typed HIR, and SSA MIR to
  ELF64 object files for its primary x86_64 target and bounded, conservative
  AArch64 G2 subset. Those native target boundaries do not apply to hosted `.O`
  execution.

`c_cpp/legacy_cpp/` is the historical C++ prototype only. It used an obsolete line-oriented JSON protocol and is not built by the active C17 Makefile or CMake paths.

## Build and run

```bash
# Rust interpreter (Cargo default-run is O)
cargo run -- examples/hello.O
cargo run -- examples/hello.O backends

# Rust binaries
cargo build --all-targets --all-features
cargo run --bin o-cli -- run examples/hello.O --no-record
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

- `crates/ostadix-api/src/parser.rs` parses typed parentheses into `ONode` trees. An identifier is an opener only if the registry accepts that language tag or alias.
- `crates/ostadix-api/src/ir.rs` lowers syntax to OIR and contains `BackendSpec` / `BackendRegistry`, the single source of truth for accepted evaluator tags, aliases, cache-safety, splice renderers, execution mode, required authorities, and shim path resolution.
- `crates/ostadix-api/src/eval.rs` executes validated OIR plans. It projects OIR to the directed HGraph and uses the state-complete graph coordinator by default. `O_EXECUTOR=serial` selects `execute_plan_serial` as the semantic differential oracle.
- `crates/ostadix-api/src/effects.rs` defines shared effect confidence, fallibility, typed resources, checked source declarations, and conservative backend classification. Unknown hosted operations use `HostWorld`.
- `crates/ostadix-api/src/executor/` contains the readiness coordinator and the worker path for verified pure inline renderers. The coordinator owns evaluator-local mutable state; it never sends the process registry across worker threads.
- `crates/ostadix-api/src/value.rs` defines `OValue`, the shared value vocabulary crossing language boundaries.
- `crates/ostadix-api/src/wire.rs` implements the hosted evaluator protocol: **4-byte length-prefixed canonical CBOR** frames.
- `crates/ostadix-api/src/hgraph/` builds and validates the directed execution HGraph. OValue, completion, resource-state, and actor-state values are nodes; evaluator invocations are multi-output hyperedges. `ReadySchedule` derives blockers only from input producers.
- `crates/ostadix-api/src/scheduler.rs` schedules autonomous request forcing and cache interaction.
- `crates/ostadix-api/src/capability.rs` models runtime authority-bearing capabilities and snapshots.
- `crates/ostadix-api/src/process.rs` manages backend shim subprocess lifetimes and requests.
- `crates/ostadix-api/src/backend.rs` contains backend protocol types shared with shims.
- `crates/ostadix-api/src/shims.rs` supports embedded/extracted shim assets for compiled hosted programs.
- `crates/ostadix-api/src/nix_ops.rs` and `crates/ostadix-api/src/nixos_ops.rs` implement Nix and NixOS operations used by builtins.
- `crates/ostadix-api/src/ocore/` contains the O-core lexer, parser, AST, HIR, type checker, MIR, codegen, driver, and capability bridge.
- Binaries: `src/main.rs` is `O`; `src/bin/o-cli.rs` owns validated intent
  `run`, `routes`, `optimize`, `plan`, `explain`, `inspect`, `object`, and
  `operation`; `olangc.rs`, `ocorec.rs`, `olink.rs` (`o-link`), `ounlink.rs`
  (`o-unlink`), `ogit.rs`, and `o-notebook.rs` provide compiler, native,
  linker, Git, and notebook entry points. The Bash `scripts/o-cli.sh`
  dispatcher remains the installed lowercase `o` front door so macOS
  case-insensitivity cannot collapse `O` and `o`.

## Environment semantics

Bare hosted blocks are ephemeral: `lang^(...)_lang` gets a fresh evaluator instance and it is discarded after the block. Persistent state is explicit only with environment-indexed tags such as `lang[0]^(...)_lang[0]` or `lang[7]^(...)_lang[7]`. The Rust evaluator represents ephemeral dispatch with `env_id == u32::MAX` and cleans that environment after execution; `SPEC.md` documents the same rule.

## Backend registry and shims

To add a Rust-hosted language backend:

1. Add a static `BackendSpec` entry to `BACKEND_SPECS` in `crates/ostadix-api/src/ir.rs`: canonical name, aliases, cache-safety flag, splice renderer, execution mode, and required authorities.
2. Add the authoritative shim under `crates/ostadix-api/backends/` when the
   backend is `ExecutionMode::Shim`, then update the root `backends/`
   compatibility mirror byte-for-byte. Shims speak the 4-byte length-prefixed
   canonical CBOR command/response protocol; the mirror-drift test rejects a
   mismatch.
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
cargo test --package ostadix-api --lib ocore::driver::tests::ocore_object_is_byte_reproducible_across_source_directories -- --exact
cargo check --manifest-path fuzz/Cargo.toml
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings

# O-core executable milestone evidence (requires Clang, LLD, Python, and QEMU x86_64)
./scripts/o-cli.sh kernel doctor
./scripts/o-cli.sh kernel build
./scripts/o-cli.sh kernel smoke
./scripts/o-cli.sh kernel smoke-live
python3 scripts/release_evidence.py validate
cargo test --test world_identity_wire
./ocore/kernel/smoke-world-identity-qemu.sh
cargo test --test world_receipt
./ocore/kernel/smoke-world-receipt-qemu.sh
./ocore/kernel/smoke-world-project-runtime-qemu.sh
./ocore/kernel/smoke-m7b-logical-read-qemu.sh
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
python3 -m unittest -v tests.test_offline_kit
python3 -m unittest -v tests.test_olang_browser_bundle
node apps/olang-browser-wasi/test-host.mjs
```

The source-release gate covers the browser host assets embedded by `olangc`.
The offline-kit suite uses fixture toolchains and vendor trees to test archive
closure, determinism, host rejection, no-clobber extraction, and tamper
detection without downloading dependencies. A real per-host kit additionally
requires the union-vendor procedure in `docs/OFFLINE_AI_BUILD_KIT.md`.

`smoke-project-hgraph.sh` is the composite hosted PR7 planning and generated
project-adapter gate. Its planning phase uses a real fixture to prove exact
bundle/policy provenance, all five project operation kinds, logical
alternative/prerequisite topology, stable nonexecuting IR/DOT output,
malformed/substitution rejection, conservative `HostWorld`, and ordinary `.O`
IR compatibility. Its World PR8-2 phase checks canonical hosted-unbound and
snapshot-derived `DeploymentPlanV1` records, exact logical/bundle binding,
bundle-scoped role/path compatibility, deterministic provider proposals, and
fail-closed unresolved cases. It also proves `scripts/o-cli.sh plan` is
byte-identical to the direct `olangc --target ir` result. After those
nonexecution checks, the smoke compiles a project binary and runs bounded
opt-in AnySuccess cases for immediate short-circuit and nonzero-to-success
continuation in disposable workspaces. The continuation fixture explicitly
declares both its executed
prerequisite and first route `failure_continuation = "declared_idempotent"`;
omitting that contract now fails closed before the second branch. `setup.sh`
installs lowercase `o` as a wrapper over the dispatcher
while preserving evaluator fallback for non-subcommand input.

`smoke-project-hgraph-exec.sh` is the separate
ProjectExec-A/ProjectExec-B ordered hosted execution gate. It proves that the
opt-in HGraph coordinator owns isolated
materialization, typed prerequisite ordering, route settlement, and unsigned
lifecycle tracing for one `Explicit`/`Default` alternative plus serial
`Fallback`/`AnySuccess`. The latter use a first-class ordered-prefix input
policy and retain each attempted result. When the terminal alternative settles
unsuccessfully, the next branch starts only if no route child executed
(guard-only skips) or every route that executed in that branch, including
successful prerequisites, explicitly declares
`failure_continuation = "declared_idempotent"`; the default `unproven` class
fails closed. A failed prerequisite remains a hard stop regardless of that
declaration because this slice has no synthesized branch-failure result. Bundle
format v2 carries the contract; a v1 bundle migrates only when all routes omit
the new field and therefore default to `unproven`. Trace v5 records the assessed
prefix, evidence class, next route, allow/deny result, and canonical
`LogicalHGraphV1` schema/digest. It also binds the exact canonical
hosted-unbound `DeploymentPlanV1` schema/digest; plan-aware replay reconstructs
that artifact and rejects its substitution. Complete traces pass
plan-aware semantic replay against the trusted HGraph, including complete
causally ordered lifecycle coverage for transitive route prerequisites;
structural replay alone does not prove bundle-bound evidence. Infrastructure
abort stops without publishing a route result. This is an HGraph-only
author-declaration gate, not
verified idempotency, a sandbox/fence/effect log, parallel race/cancellation,
retry,
snapshot-plan execution, actual placement, runtime/recovery graphs,
Governor/receipt integration, exactly-once effects, native or QEMU evidence,
hardware isolation, G1, or G0--G13 passage.

`crates/ostadix-api/src/project/logical.rs` is the World PR8-1 project profile. It derives
`LogicalHGraphV1` after validating the exact plan-to-HGraph projection,
preserves residual `HostWorld`, rejects unknown schema data, offers a separate
strict canonical decoder, and supplies the logical digest used by trace v5.
The digest binds exact bundle bytes and metadata; it normalizes logical-record
JSON encoding, not source or manifest formatting. It does not itself define
placement, runtime, recovery, World task identity, authority grants, receipts,
or G1 evidence.

`crates/ostadix-api/src/project/deployment.rs` is the bounded World PR8-2 intention layer. Its
ordinary hosted constructor binds supported coordinator operations only to
`HostedCoordinator` or `AmbientHost`, carries no World/task/provider identity,
and leaves unsupported hosted policies `Unresolved`. Requirements actively
derive the exact project bundle and bundle-scoped role/path declarations,
runtime classes, executable/evaluator facts, platform and ambient-environment
guards, authority absence, and residual `HostWorld` admission. Bundle
environment overlays are recorded separately; architecture, package, and
failure-domain fields are currently unconstrained or empty schema vocabulary.

The snapshot constructor requires an exact caller-supplied
`PlacementSnapshotV1` and exact caller-supplied `TaskIdentity` map, then derives
a deterministic single-provider `ProposedProvider` or `Unresolved` record.
Snapshot/provider facts are descriptive and do not prove current inventory,
Governor admission, authority, dispatch, reservation, health, or execution.
`require_current_world` checks only World identity/epoch. The ordinary opt-in
executor binds the canonical hosted-unbound plan into unsigned trace v5. The
separate hosted-reference World path instead consumes the exact
snapshot-derived plan through `ProjectCoordinator::new_world_bound`.

`crates/ostadix-api/src/project/launch.rs` defines the non-authorizing `HostedWorldLaunchV1` and
caller-supplied `HostedWorldCurrentV1` boundary. It re-derives the logical,
deployment, snapshot, provider, and operation task bindings and fences the exact
World/Governor; caller-supplied coordinator observer
node/domain/optional-process; dedicated coordinator attempt; provider
node/domain/optional-process/service generations and implementation digest; and
every operation task attempt before schedule derivation, workspace
materialization, or child launch. The coordinator attempt must use a task
identity distinct from all per-operation attempts and is the World-bound trace
execution-attempt identity. These identity comparisons do not authenticate
membership, prove the host process owns the observer identity, grant a
capability or lease, reserve a provider, or record Governor admission.

`crates/ostadix-api/src/project/runtime_graph.rs` builds terminal `RuntimeGraphV1` from the exact
launch artifacts only after plan-aware causal replay of the normalized
`ProjectAttemptTrace` against the trusted HGraph and exact deployment. It
retains empty observations for never-started operations and records lifecycle
ordinals, settlement/output hashes, and per-operation residual `HostWorld`.
The neutral `RouteSettlement` terminal covers success, nonzero settlement, and
guard skip; aggregate terminal residual `HostWorld` is true when any actually
observed started/terminal operation retains it. The
`crates/ostadix-api/src/project/world_execution.rs` adapter emits one canonical OWRECEIPT using the
caller's Ed25519 signer and always sets
`ReceiptCommitFenceV1::Uncommitted`. Receipt placement is the coordinator
observer and the receipt context uses the dedicated coordinator attempt, not the
proposed provider or a per-operation attempt. The subject leaves package absent
instead of overloading it with the provider implementation. Only a successful
route produces receipt success; nonzero and guard-skipped settlements produce
receipt failures. Signature integrity is not Governor authority or a governed
commit.

Mode 32 performs full native canonical receipt decode, exact re-encoding,
validated signing-preimage construction, uncommitted-fence checking, and a
domain-separated unsigned-body semantic SHA-256 comparison. The required
no-argument wrapper generates a receipt through the hosted test and passes it
to the direct two-argument vector interface. The native probe also reuses its
successful validation scratch on a malformed envelope and requires prior
terminal/commit tags to have been reset:

```bash
./ocore/kernel/smoke-world-project-runtime-qemu.sh
./ocore/kernel/smoke-world-project-receipt-qemu.sh RECEIPT_HEX_FILE EXPECTED_SEMANTIC_SHA256
```

This slice has no Governor admission/commit, capability/lease issuance,
reservation, remote dispatch, recovery, or exactly-once protocol. Mode 32 does
not execute the project or verify Ed25519 natively; QEMU TCG is not physical
hardware. It passes neither G1 nor Workstream A acceptance, and G1 remains
defined and unpassed.

Keep the installed-wrapper directories before `target/release` in `PATH`; on a
case-insensitive host the raw `O` release binary is otherwise also found as
lowercase `o` and shadows the dispatcher.

<!-- BEGIN GENERATED: REQUIRED_QEMU_EVIDENCE_DEVELOPMENT -->
The aggregate executes all 26 required portable QEMU gates in the
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
- Keep `OWRECEIPT` v1 claims precise: PR 5/Mode 30 remains the bounded offline
  canonical corpus and hosted pinned-key conformance oracle. The separate
  World-project hosted-reference path accepts a caller signer and emits a live
  canonical receipt, but its commit fence is always `Uncommitted`. Mode 32 fully
  decodes/re-encodes that record, reconstructs its signing preimage, and proves
  stale terminal/commit tags are cleared when validation scratch is reused;
  it does not verify Ed25519 or execute the project. Receipt placement names
  the caller-supplied coordinator observer, while the proposed provider remains
  descriptive and the package subject stays absent. None of these paths
  supplies production key custody, trusted signer policy, Governor
  admission/commit, authoritative fencing, Workstream A acceptance, G1, or
  G0--G13 passage.
- Keep hosted ResourceKey PR6 claims precise: its smoke proves typed planner
  vocabulary, underlying identity helpers' caller-pair comparison, HGraph state
  chaining, alias-aware grounding projection, source-forgery rejection, and
  residual `HostWorld`. Grounding only checks the bound World epoch/membership.
  This is not Mode 31, a wire ABI, production governed lowering, native/QEMU
  evidence, Governor authority, device assignment, DMA/IOMMU isolation,
  Acceptance A, or G0--G13 passage.
- Keep HGraph wording precise: graph dispatch is implemented, but worker-pool execution is limited to verified pure inline renderers. Unknown shims are serialized through `HostWorld`, and exact arbitrary-source filesystem/network inference is not implemented.
- Prefer registry metadata over duplicated backend-name, purity, renderer, or authority tables.
