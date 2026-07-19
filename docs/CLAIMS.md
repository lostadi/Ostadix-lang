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
- The source-release builder in `scripts/build_source_release.py` reads only
  allowlisted blobs from a resolved Git commit, rejects dirty worktrees by
  default, emits a deterministic ZIP with a canonical manifest and checksums,
  and self-verifies before atomic publication. Its regression suite covers
  debris exclusion, reproducibility, committed-byte behavior, and tampering.
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
- O-core Milestones 0.1 through 0.3 are complete for their bounded, single-CPU
  QEMU bootstrap gates. They prove CPL3 `SYSCALL` and IRQ return, page-granular
  kernel and user protection, normalized faults, fault-aware bounded user copy,
  and a reclaiming typed registry for all 3,072 frames in the fixed 4..16 MiB
  window. The default gate also proves generation-safe memory-object reuse,
  zero-before-reuse, rollback, per-CSpace quotas, and capability-returning
  anonymous/shared `page_alloc`. The fresh-boot fault matrix remains evidence
  about the one-process Milestone 0.2 scenario, not the current upper bound.
- Milestone 1 is complete for two bounded native processes on one CPU.
  `smoke-processes-qemu.sh` runs independent exit and fault scenarios and proves
  separate CR3s, the same private virtual address backed by different frames,
  atomic PCB/domain/address-space/CSpace switching, split teardown, stale handle
  denial, sibling survival, complete dynamic-frame reclamation, and a later
  timer marker.
- Milestone 2 is complete for four TCBs across two processes on one CPU.
  `smoke-scheduler-qemu.sh` proves one million forced identity transactions, FIFO
  runnable and blocked queues, two CPU-bound and two sleeping CPL3 threads,
  cooperative yield, timer preemption, wake-once sleep, bounded priority
  accounting, an idle path, cross-thread hostile-RFLAGS sanitization, hostile
  saved-RSP TCB containment, exit during preemption, sibling progress, stale
  TCB denial, frame reclamation, and post-lifecycle timer survival. The million
  transactions do not enter CPL3; real frame save/restore and IRETQ switching
  are proved separately by the bounded IRQ/SYSCALL phase.
- Milestone 3 has a verified foundation, not a completed IPC milestone.
  `smoke-ipc-foundation-qemu.sh` exercises generation-tagged endpoints with a
  four-message FIFO, cancellation and waiter-record cleanup, invisible
  destination-slot reservations, shared-memory-only attenuating transfer
  tickets, exact-generation queued-capability escrow, and one optional fixed
  RW/NX shared page in each of two address spaces. Shared mapping requires a
  `RIGHT_MEMORY_USE` capability from the address space's exact owner CSpace. The
  gate proves cross-CR3 visibility, exact attenuation, failed re-transfer,
  generation reuse, rejection of new work from a dead sender, explicit
  management-harness cancellation of its earlier queue item and ticket,
  reclamation, and later timer survival. The public `cap_copy` number still
  returns `ERR_NOT_IMPLEMENTED`. The waiter records are registry bookkeeping,
  not live blocked TCB transitions. No CPL3 endpoint ABI, real blocked IPC,
  preemptive request/reply ping-pong, complete sender/receiver death matrix, or
  personality-service crash-containment gate exists yet.

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
- O-Domain evolution beyond the current Milestone 3 foundation: a CPL3 IPC ABI,
  blocking send/receive, preemptive request/reply and death cleanup, supervised
  personality services, executable loaders, service discovery, the native live
  system in `docs/LIVE_SYSTEM.md`, and only then a minimal Linux x86_64
  personality. The staged engineering plan is in `docs/ODOMAIN_PLAN.md`; its
  required foreign-process memory boundary is in
  `docs/PERSONALITY_MEMORY_VIEW.md`.
