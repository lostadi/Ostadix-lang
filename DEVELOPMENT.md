# Ostadix-lang Developer Guide

Practical notes for contributors to [Ostadix-lang](https://github.com/lostadi/O-lang), short name **O-lang**. The project is a polyglot language system where typed expressions choose their evaluator with `LANG^(...)_LANG`, and O-core is the separate freestanding native systems language.

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
- `src/eval.rs` executes validated OIR plans. It projects OIR to HGraph, solves and schedules that projection, checks it against the OIR plan, then arbitrary evaluator operations still run through the serial `execute_plan_serial` executor.
- `src/value.rs` defines `OValue`, the shared value vocabulary crossing language boundaries.
- `src/wire.rs` implements the hosted evaluator protocol: **4-byte length-prefixed canonical CBOR** frames.
- `src/hgraph/` builds HGraph from OIR, solves type/fidelity facts, clusters nodes, and derives schedules. General concurrent graph dispatch is not implemented.
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

# Python reference runtime
python3 -m tests.test_parser
python3 -m tests.test_evaluator
python3 -m compileall -q o_lang backends tests

# C17 interpreter and AOT compiler
make -C c_cpp test
make -C c_cpp olangc-test
cmake -S c_cpp -B build/c_cpp-cmake
cmake --build build/c_cpp-cmake
ctest --test-dir build/c_cpp-cmake

# Documentation/release-claim guard
bash scripts/check_release_claims.sh
```

The byte-reproducible O-core object test protects deterministic native object output across source directories. `cargo test --test parser_proptest` runs parser proptest properties, and `cargo check --manifest-path fuzz/Cargo.toml` ensures the parser fuzz target still compiles.

## Conventions

- Keep the Rust implementation authoritative for hosted `.O` semantics; keep Python readable and C17 active rather than treating either as abandoned.
- Keep wire-protocol claims precise: hosted shims use 4-byte length-prefixed canonical CBOR.
- Keep HGraph wording precise: lowering, type/fidelity solving, clustering, and schedule derivation exist; general concurrent graph dispatch does not.
- Prefer registry metadata over duplicated backend-name, purity, renderer, or authority tables.
