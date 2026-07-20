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
- The hosted Live-World reference in `src/live_system/` and
  `src/bin/o-live-host.rs` implements strict bounded package ingestion, an
  immutable verified SHA-256 store, exact default-deny activation policy,
  per-service child supervision, health-gated transactional publication,
  generation-bound private service bearers, rollback, targeted restart,
  active-set reconstruction, and sequential cross-world composition through
  pure, boot-persistable OValues. `scripts/smoke-hosted-live-reference.sh`
  exercises the complete hosted scenario. On Unix, each stateful CLI command
  holds a process-shared advisory lock from before any reconstruction or
  mutation through its complete operation. Direct supervisor mutations also
  use a persisted monotonic revision and an active-set-specific process lock:
  publishing activation, rollback, and service restart compare and advance the
  observed revision, while a stale API instance is rejected and must
  reconstruct. `tests/hosted_supervisor_transactions.rs` exercises that
  two-writer conflict as well as read-only, generation-preserving
  reconstruction.
- The hosted Live-World reference is not a native O-core service manager. Its
  workers use host processes and pipes, its state directory is same-user
  trusted control-plane authority, and arbitrary host syscalls are not fully
  contained. It is a differential semantic oracle, not evidence for any native
  QEMU claim below; those claims have their own gates.
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
- Milestone 3 now has a bounded native IPC gate in
  `ocore/kernel/smoke-ipc-qemu.sh`. Four CPL3 processes exercise public endpoint
  create/send/receive/cancel syscalls, cross-domain request/reply, a full
  four-message FIFO with real TCB blocking and wake-once retry, exact
  attenuation during capability transfer, automatic dead-sender cleanup, and
  exception-driven personality crash containment while an unrelated world
  continues. A ticket is bound to its exact creating process generation and
  the destination CSpace derived from the authorized endpoint, not to the
  endpoint object. The gate fills all 16 ticket slots, denies abort by another
  process, lets the owner unwind them exactly once, rejects the stale ticket,
  and proves allocation recovers. It also requires transactional teardown,
  complete resource reclamation, one later timer interrupt, and one second of
  QEMU survival. The earlier `smoke-ipc-foundation-qemu.sh` remains a narrower
  regression gate; `cap_copy` is now the attenuation-only transfer-ticket
  operation, while its legacy all-zero capability probe still returns
  `ERR_NOT_IMPLEMENTED`.
- Milestone 4 has a bounded native loader/VFS gate in
  `ocore/kernel/smoke-loader-qemu.sh`. `build-m4-artifacts.sh` rebuilds two
  independently linked static O-core ELF personalities and a deterministic
  read-only `OVFSIMG1` image, checks deterministic repacking and SHA-256 on the
  host, and includes malformed, overlapping, and W+X ELF corpus entries. A
  fresh QEMU boot imports the image as data, rejects that corpus before start,
  and loads both personalities into separate W^X address spaces at the same
  preferred user window, resolves a service to an attenuated capability, tears down the
  namespace transactionally, reclaims every dynamic frame, and reaches a later
  timer. The kernel runs NIST-vector-tested SHA-256 over the complete embedded
  bytes before import and rejects any mismatch with the canonical artifact
  identity; the host independently verifies reproducibility and the same digest.
- Milestone 5 has a bounded native live-system gate in
  `ocore/kernel/smoke-live-qemu.sh`. `build-m5-artifacts.sh` deterministically
  builds separate static `init`, supervisor, package-daemon, and REPL ELF files
  into a 62,056-byte content-addressed read-only OVFS image whose pinned SHA-256
  is `88c0db7b97f74b091407731a0be8d9bf25c86f0ca03aaf8040b2b7c007cb9fed`.
  The host verifies that identity and the kernel recomputes it before import.
  The fresh-QEMU gate proves four isolated loaded CSpaces and a real CPL3 serial
  loop whose only serial-read and control-submit authority is a typed control
  capability. The host sends a malformed command, an exact immutable-digest
  install, and activation in that order; the malformed command cannot publish
  state, while activation publishes all four service records only after exact
  capability grants and concrete process health tokens. The package daemon then
  deliberately faults in CPL3. The gate contains that one generation, preserves
  the three unrelated services, withdraws the stale service while the control
  state is `CONTROL_RECOVERING`, rejects stale process/thread/CSpace/address-space/
  capability/service generations, and runs a freshly loaded package-daemon
  generation. Only its exact restart health token permits republication. The
  gate then deactivates the control plane, revokes the control capability, tears
  down its namespace and processes, reclaims all frames, observes a later timer,
  and requires another second of QEMU survival.
- The independent mode-17 gate in `smoke-live-semantics-qemu.sh` executes the
  bounded package/supervisor state corpus: two immutable roots, overgrant and
  incomplete-set denial, failed-health nonpublication, complete-set rollback,
  stale references, abstract crash/restart with unaffected state, and strict
  serial parsing. Neither native gate establishes a general or unbounded retry
  or backoff policy. A mode-16 replacement that fails before publishing its exact
  health token remains withdrawn and fails closed; recovery from that second
  fault is not claimed.
- M6A has a bounded package-loaded scalar personality-supervision gate in
  `ocore/kernel/smoke-personality-qemu.sh`. `build-m6-artifacts.sh` rebuilds
  four static ELFs at `/sbin/m6-client.elf`,
  `/sbin/m6-personalityd.elf`, `/sbin/m6-supervisord.elf`, and
  `/sbin/m6-observer.elf`, requires byte identity, and packs exactly those paths
  into a 62,104-byte read-only OVFS image with SHA-256
  `c2699a2eadae2b406a0b48ecec424fda0cb36402f7cac7324441d98aff73c4e7`.
  The host gate checks that identity and proves the user modules are not linked
  as kernel code; the kernel verifies the complete image before import. Mode 18
  loads all four into isolated W^X address spaces and CSpaces. The unprivileged
  supervisor health-gates generations 1 and 2, requests publication, selects
  cancellation, requests one crash-driven restart, and requests cooperative
  stop. O-core installs and rotates the client call capability. The
  endpoint-backed scalar router proves the
  ping/add-one/unsupported corpus plus deterministic cancellation, timeout, and
  service-death results with one terminal wake. It rejects late, duplicate,
  prior-generation, and stale-capability use while an unrelated observer keeps
  progressing, then reclaims every dynamic resource and reaches a later timer.
  This is M6A rather than full Milestone 6: pointer-bearing calls and foreign
  memory views remain disabled, and it establishes no Linux or other foreign
  ABI.

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
- O-Domain evolution beyond the current bounded native gates: extend M6A's
  unprivileged scalar personality and supervision loop with durable reboot
  reconstruction and a capability-bounded build service, then implement the
  request-scoped foreign-process memory boundary in
  `docs/PERSONALITY_MEMORY_VIEW.md`. No Linux ABI or root filesystem is claimed.
  The staged engineering plan is in `docs/ODOMAIN_PLAN.md`.
