# Claim-accuracy inventory

## Implemented and tested now

- Expression-granular recursive evaluator composition is implemented by typed
  expression syntax described in `README.md`, lowered from parser nodes to OIR in
  `src/ir.rs`, and executed by the Rust evaluator in `src/eval.rs`.
- The accepted evaluator tags are registry-extensible at compile time through
  `BackendRegistry` and `BACKEND_SPECS` in `src/ir.rs`; this table is the single
  source for accepted canonical tags, aliases, purity metadata, splice
  rendering, execution mode, shim fallback, and backend authority requirements.
- `OValue` is the language-neutral value boundary (`src/value.rs`) used by the
  Rust hosted runtime, the C17 edition in `c_cpp/`, and the Python reference in
  `o_lang/`.
- The hosted process protocol uses a 4-byte big-endian length prefix followed
  by canonical CBOR encoding in `src/wire.rs`; maps are sorted by encoded key
  length and bytes before transmission.
- The repository contains three hosted implementations: the Rust authoritative
  runtime (`src/`), the C17 interpreter and AOT `olangc` (`c_cpp/Makefile` and
  `c_cpp/CMakeLists.txt`, both using C17), and the Python reference edition
  (`o_lang/`). It also contains O-core freestanding x86_64 ELF object emission
  through `ocorec` (`README.md`, `src/bin/ocorec.rs`, `src/ocore/driver.rs`).
- Hosted evaluation lowers to OIR, builds and validates an `ExecutionPlan`, and
  projects it into a directed state-complete HGraph. The graph coordinator is
  the default executor; `O_EXECUTOR=serial` retains the topological OIR
  interpreter as a differential oracle.
- HGraph represents ordinary results, successful completion, evaluator state,
  host-resource state, and persistent actor state as nodes. Executable
  operations are directed, multi-output hyperedges. Readiness follows only from
  materialized inputs and their producers.
- Unknown hosted operations are conservatively serialized through a shared
  `HostWorld` state chain. Persistent environments also use typed actor-state
  chains. The implementation does not claim exact effect inference from
  arbitrary Python, Bash, JavaScript, Rust, or other hosted source.
- Conservative `{lazy}` cache safety is enforced from backend metadata in
  `src/ir.rs` and validation in `src/eval.rs`: inline `html`, `markdown`,
  `latex`, and `text` are cache-safe; unrestricted shim backends including
  `nix`, `sql`, `haskell`, `ocaml`, and `webassembly` are rejected before shim
  execution when `{lazy}` is requested.
- Capability and authority checks exist for hosted backend execution and system
  activation (`src/capability.rs`, `src/eval.rs`). Backend processes are keyed
  by sandbox policy in `src/process.rs`, and backend dispatch checks requested
  authorities before execution.
- Supported concurrent request classes are the scheduler's threadable Nix-family
  request kinds: instantiate, realise, and dry activation. Group modes
  `batch`, `all`, `any`, and `race` are represented in `src/value.rs`, lowered
  through `src/ir.rs`, and resolved by evaluator/scheduler code; Eval requests
  remain serial.
- Native value crossings are conservative: `Fidelity::NativeCapsule` in
  `src/value.rs` and `src/hgraph/solve.rs` prevents claiming general
  cross-runtime native value soundness.
- The bootable O-core proof runs one statically linked `native[0]` process at
  CPL3. The x86_64 bootstrap installs user/supervisor mappings, NX and write
  protection, a 64-bit TSS, and `SYSCALL` MSRs. Kernel-owned PCB identity
  selects the native personality and process cspace; QEMU verifies valid debug
  output, capability bounds/occupancy/generation/type/right checks,
  stale-after-reuse and closed-handle denial, complete user-range checks,
  ELF user-region zero-fill, kernel-pointer denial, hostile-RFLAGS
  sanitization, unknown-syscall denial, the future scheduler's yield hook, an
  IRQ0 privilege transition and return, and a later CPL3 heartbeat. This does
  not claim an executable loader, independent address spaces, scheduling, IPC,
  Linux compatibility, or foreign root filesystems.

## Implemented conservatively

- Graph construction, multi-output validation, type/fidelity solving, explicit
  resource and completion lowering, scheduling, and readiness-driven dispatch
  are implemented in `src/hgraph/`, `src/effects.rs`, and `src/executor/`.
- Parallel dispatch is correctness-first. Only verified pure, deterministic,
  infallible, state-free inline renderers run on workers. Arbitrary Eval/shim
  operations stay on the evaluator owner thread and are ordered by graph state.
- Ordinary source sequence is preserved by completion dependencies unless an
  explicit concurrent group or the narrow verified-inline rule removes it.
- Full N-language communication soundness is not established; native OValue
  crossings remain conservatively represented as `NativeCapsule`.

## Research directions enabled by the architecture

- Parallel read leases and verified path-, endpoint-, and service-specific
  resource models that safely reduce `HostWorld` serialization.
- Runtime plugin registration beyond the current static `BackendRegistry` table
  in `src/ir.rs`.
- Fingerprint-complete effect tracking and verified backend analyzers that could
  safely broaden `{lazy}` and graph parallelism beyond trusted inline backends.
- More precise backend morphism proofs and fidelity accounting for OValue
  crossings, extending the current `Fidelity` and `BackendMorphism` vocabulary
  in `src/value.rs`.
- Deterministic cancellation and result-selection semantics for concurrent
  groups and future graph execution.
- O-Domain evolution beyond `native[0]`: independent per-process address
  spaces, preemptive scheduling, capability transfer, IPC, executable loaders,
  service discovery, and only then a minimal Linux x86_64 personality. The
  staged engineering plan is in `docs/ODOMAIN_PLAN.md`.
