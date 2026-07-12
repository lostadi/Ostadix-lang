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
- Hosted evaluation lowers to OIR, builds an `ExecutionPlan`, validates the
  plan, projects it into HGraph for analysis, derives a schedule, and then
  interprets the OIR in topological order through `execute_plan_serial` in
  `src/eval.rs`.
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

## Implemented as representation/analysis but not general dispatch

- HGraph construction from OIR exists in `src/hgraph/from_oir.rs` and is
  invoked from `OIrProgram::hgraph_for_plan` in `src/ir.rs`.
- HGraph validation through the source `ExecutionPlan`, type/fidelity solving,
  actor constraints, group/control edges, clustering, and schedule derivation
  exist in `src/hgraph/graph.rs`, `src/hgraph/kinds.rs`, `src/hgraph/solve.rs`,
  and `src/hgraph/schedule.rs`.
- The derived HGraph schedule is currently checked against the OIR root schedule
  in `src/eval.rs`, but arbitrary evaluator operations still dispatch through
  the serial OIR executor `execute_plan_serial`.
- General arbitrary-Eval HGraph parallel dispatch is not implemented.
- Full N-language communication soundness is not established; native OValue
  crossings remain conservatively represented as `NativeCapsule`.

## Research directions enabled by the architecture

- Actor-oriented general graph dispatch over persistent evaluator actors, as
  scoped in `docs/HGRAPH_EXECUTOR_PLAN.md`.
- Runtime plugin registration beyond the current static `BackendRegistry` table
  in `src/ir.rs`.
- Fingerprint-complete effect tracking that could safely broaden `{lazy}` beyond
  the current inline cache-safe backends.
- Cross-runtime schedule optimization using HGraph data, sequence, actor,
  effect, and authority edges.
- More precise backend morphism proofs and fidelity accounting for OValue
  crossings, extending the current `Fidelity` and `BackendMorphism` vocabulary
  in `src/value.rs`.
- Deterministic cancellation and result-selection semantics for concurrent
  groups and future graph execution.
