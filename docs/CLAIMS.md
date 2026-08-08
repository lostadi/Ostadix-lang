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
  `ocorec` now also emits a conservative freestanding AArch64 scalar subset
  through `src/ocore/codegen_aarch64.rs`; unsupported atomics, inline assembly,
  interrupt/naked functions, floating point, and calls exceeding eight integer
  arguments fail closed.
- `examples/manifest.json` completely classifies the checked-in `.O` example
  tree by supported edition, unit/integration/manual status, runtime and
  authority requirements, timeout, and edition-specific oracle. The Rust,
  Python, and C17 test entrypoints consume it. In particular, an unsupported
  backend producing literal text or a zero-exit run containing a fatal shim
  diagnostic is not accepted as example conformance, and an all-skipped sweep
  fails. Rust/C17 oracles are observable output patterns; exact OValue JSON is
  supported only by the Python semantic runner. Manifest authorities describe
  ambient host requirements and do not mint a capability.
- The source-release builder in `scripts/build_source_release.py` reads only
  allowlisted blobs from a resolved Git commit, rejects dirty worktrees by
  default, emits a deterministic ZIP with a canonical manifest and checksums,
  and self-verifies before atomic publication. It accepts only regular
  non-executable or executable Git blobs, so a symlink cannot become an escaping
  archive member. Relative Markdown links outside code/comments must resolve to
  files, directories, or the root inside that archive. Verification also checks
  canonical ZIP metadata/layout and inertly validates the MCP configuration,
  crate license, example/evidence manifest schemas, sealed World Alpha
  constitution-v3/profile/composed-contract/registry-v4 bytes, exact historical
  attestation and ledger-event bytes, current released-source digests, and
  archive-local references; it never imports or executes archive payloads.
  Schema-v2 historical source sets are one coherent snapshot at commit
  `c25d38c00283f2873eed1aa84dd89b437777e356`; reconstructing those bytes
  requires full Git history and is explicitly not archive-only proof. The supported local
  MCP crate, its lockfile, LGPL-2.1 license, repository config, and stdio smoke
  regressions are required release members. The regression suite covers debris
  exclusion, link closure, reproducibility, committed-byte behavior, required
  release surfaces, metadata/schema tampering, and symlink denial.
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
- `src/kernel_world.rs` implements a strict, bounded
  `ocore.kernel-world/v1` manifest and a host-side lifecycle oracle shared by
  source-integrated and binary-contained foreign-kernel provider designs.
  `tests/kernel_world_contract.rs` proves unknown-field rejection, canonical
  parsing, exact binding to verified package metadata, byte verification for
  package-payload images, expected-digest constraints for user-supplied images,
  execution-mode constraints, unique request kinds, exact
  device-export-to-`device.*` authority binding, and distinct-bound-authority
  `max_devices` accounting. Multiple exports may share one authority request.
  Reserved rights are kind-typed: `vm.machine` accepts `run|stop`, while
  `device.*` accepts `reset|dma`. The gate also proves health-gated export
  resolution, bounded request admission, one-terminal-result behavior, failure
  fan-out, restart policy, provenance, and stale-generation denial. Its
  `OKWORLD1` V2 encoder produces a bounded, deterministic native normal form
  only from a `VerifiedKernelWorld`; the decoder returns a distinct
  inspection-only type and rejects malformed, noncanonical, V1,
  all-zero-package, or over-limit records. Decoding cannot recreate
  verified-package authority.
- Mode 20 carries that contract into a bounded native supervisor-admission/
  object gate. Its V2 fixture record is exactly 459 bytes with SHA-256
  `0ece5f7f37ebe203d03cc7e5213dc8f9257a9a225a73e52d37d1f718424b9232`
  and exact canonical requirements `["npt", "svm"]`.
  `kernel_world_record.oc` verifies the embedded record's exact SHA-256 before
  strict parsing, including exact export-authority keys, unique request kinds,
  typed rights, distinct-authority quota accounting, and byte-exact backend
  requirements. `kernel_world_admission.oc` preserves separate package and
  manifest digests and applies independently registered default-deny policy
  keyed by exact package digest and copied byte-exact request kind/purpose;
  hashes cannot authorize. `vm_object.oc`
  creates generation-bound, nonexecuting VM/vCPU identities and aligned guest
  pages backed by anonymous 4 KiB memory objects. The gate proves quota,
  overlap, stale-generation, exact-world revoke/reclaim, unrelated-VM survival,
  and a later timer. It does not execute a foreign kernel, enter VMX/SVM,
  construct EPT/NPT, run firmware, inject interrupts, publish a provider export,
  assign a device, map DMA, or provide IOMMU isolation.
- Mode 21 is an AMD-only executable substrate gate. The host requires KVM plus
  `svm` and `npt`, and the kernel compares every retained requirement byte
  against exactly `["npt", "svm"]` before SVM initialization. It executes only
  a two-page real-mode synthetic guest through a private NPT, with bounded
  interrupt injection, a controlled hypercall, an unmapped-GPA denial, exact
  teardown, stop/restart, and unrelated-VM survival. Modes 20 and 21 provide no
  live device service or device capability, guest agent, shared queue or shared
  ring, Linux or Plan 9 boot, virtual device, PCI assignment, IOMMU isolation,
  DMA mapping, device reset, or 9P implementation.
- Mode 22 is a separate QEMU-TCG native administrative lifecycle gate.
  `kernel_world_boot.oc` binds at most two admitted worlds to configured VM
  identities and exact consumer CSpaces, requires the independently granted
  `vm.machine:run` authority for administrative start, and health-gates at most
  four generation-tagged export capabilities by exact protocol ID. Client
  status returns the native boot generation. A device-plane reset right accepts
  broker intent only when derived from its exact independently granted
  `device.*:reset` request; it neither transfers the provider grant nor resets
  hardware. Failure withdraws bindings and closes capabilities before exact
  VM-graph revoke, then the declared `on_failure` policy may authorize a fresh
  VM/boot/service generation while unrelated state survives and stale handles
  fail. Duplicate consumer-CSpace/name/protocol ID tuples are denied.
  `SYS_CAP_CLOSE` retires the registry binding with the capability, and closing
  the last export returns the boot to `HEALTHY`. Terminal uninstall revokes
  admission before consuming the tombstone only after proving the exact local
  VM graph is absent; an un-staged replacement makes uninstall fail unchanged.
  It exposes no separate abandon operation. Lifecycle/broker transitions use
  single-CPU operation ownership and linearization epochs; future SMP requires
  an atomic lock. The gate calls health and failure directly, does not enforce
  the declared health timeout, and starts no process, guest, or foreign
  provider. It supplies no guest agent, shared ring/queue, 9P, PCI/device
  assignment, IOMMU/DMA isolation, physical reset, Linux boot, or Plan 9 boot.
- Mode 23 is the bounded execution-and-device composition gate. QEMU TCG
  emulates an x86-64 CPU exposing AMD SVM/NPT; the result is architectural
  guest entry and VMEXIT under emulation, not KVM, physical AMD execution, or
  hardware isolation. One generation-tagged session binds the exact boot,
  admitted-world generation, configured VM, current vCPU, code/mailbox pages,
  device export, and independently granted request. A cross-world vCPU is
  denied before activation, and an execution pin blocks VM-graph teardown
  while SVM owns retained mappings.
  An exact `VMMCALL` supplies health, while the coordinator derives the health
  protocol from the admitted world rather than guest data. The fixed synthetic
  guest then executes one validated 32-bit `OUT` to port `0xE0`; only its
  generation-bound kernel-internal endpoint receives the scalar and returns
  `input XOR 0xA5A55A5A`. The published reset-request capability dispatches to
  that exact endpoint and clears software transaction state only.
  A deliberate NPF synchronously orders SVM/NPT stop and mapping/pin release,
  virtual-endpoint revocation, client withdrawal, and exact VM-graph
  revocation. An unrelated published service survives. The `on_failure`
  replacement uses fresh generation-2 VM, boot, session, endpoint, and client
  identities; generation-1 authority remains stale.
  Mode 23 does not boot Linux, Plan 9, firmware, or a supplied user image. It
  has no general guest agent, shared queue/ring, asynchronous or SMP guarantee,
  physical PCI/device assignment, DMA/IOMMU isolation, interrupt remapping, or
  hardware reset. Its TCG result is not KVM or physical-hardware evidence.
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
  is `388b9253ce6f92bef1e1f986b46aabbeb728604cc73589d12105031f5f6b780a`.
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
  `f5924eeb64b5a3d332e20b5d0fae7b233ae2714eb58b72ea07f08a4d26334417`.
  The host gate checks that identity and proves the user modules are not linked
  as kernel code; the kernel verifies the complete image before import. Mode 18
  loads all four into isolated W^X address spaces and CSpaces. The unprivileged
  supervisor health-gates generations 1 and 2, requests publication, selects
  cancellation, requests one crash-driven restart, and requests cooperative
  stop. O-core installs and rotates the client call capability. The
  endpoint-backed scalar router proves the
  ping/add-one/unsupported corpus plus deterministic cancellation, timeout, and
  service-death results with one terminal wake. It rejects late, duplicate,
  prior-generation, and stale-capability use while retaining all nine consumed
  terminals in a 16-record exact-handle history with zero eviction. Its fault
  watch is FIFO-queued before cancellation releases the client. An unrelated
  observer keeps progressing, then the gate reclaims every dynamic resource
  and reaches a later timer.
  This is M6A rather than full Milestone 6: pointer-bearing calls and foreign
  memory views remain disabled, and it establishes no Linux or other foreign
  ABI.
- M6B has a separate first native mechanism gate in
  `ocore/kernel/smoke-m6b-qemu.sh`. Mode 19 implements four
  generation-tagged, request-scoped bounded-copy views with kernel-owned
  staging, direction-attenuated nontransferable capabilities, a 128-byte
  per-view and 256-byte aggregate quota, snapshot input, and written-prefix-only
  output commit after exact process/address-space revalidation. Reply,
  cancellation, timeout, service-death, process-exit, unmap, and delegated
  revocation hooks close capability authority before one terminal disposition
  and one wake publication. Post-reply process-exit/unmap cleanup releases an
  undeliverable terminal view without a second disposition or wake. Typed
  generation-tagged leases carry nonzero request identities, cover memory,
  filesystem, timer, network, and device classes, and support request-wide
  revocation without ambient fallback while unrelated requests survive.
  Create-plus-bind is transactional, with an injected post-publication bind
  failure proving capability revocation and exact-generation destruction. These
  are directly exercised terminal hooks, not yet integration with live process
  teardown, mapping mutation, or scheduler wake. This is not complete M6B: the
  gate is not wired
  through the CPL3 personality daemon or public pointer-bearing RPC, and it has
  no pinned windows, streaming mode, signal/restart integration, Linux oracle,
  schema fuzzing, allocation-failure matrix, or concrete filesystem, network,
  timer, or device implementation.
- Mode 24 adds a bounded live M6B composition gate in
  `ocore/kernel/smoke-live-bounded-personality-qemu.sh`. Four independently
  linked CPL3 principals are rebuilt into a 65,152-byte OVFS image with
  SHA-256
  `5b9d2526da2abd75ec90b4770ded5923d856132fad736fb13f241c34f1579887`.
  One exact four-byte `INOUT` request shape crosses the public bounded-call,
  view lookup/read/write, and bounded-reply syscalls without client reissue.
  The gate contains a generation-1 daemon fault, preserves the unrelated
  observer, health-gates a generation-2 rebind, and covers cancellation,
  timeout, service death, plus supervisor-triggered pre-terminal unmap,
  request-revoke, delegated-device-resource-revoke, and caller-exit
  dispositions. The latter are policy-triggered while requests are waiting:
  this is not evidence of actual mapping mutation, an external resource event,
  or the post-reply/pre-consume process-exit or unmap race. The delegated
  device authority is one internal typed lease, not a physical device. Mode 24
  is not a Linux or Plan 9 boot, general foreign ABI, general guest agent, KVM,
  PCI/DMA/IOMMU, or physical-device isolation gate.
- Mode 25 adds the first bounded live foreign-ABI composition gate in
  `ocore/kernel/smoke-live-linux-personality-qemu.sh`. One exact 8,520-byte
  static Linux x86-64 ELF and three native service principals are rebuilt into
  a 60,104-byte OVFS image, pinned independently by SHA-256, loaded as data,
  and entered at CPL3. The foreign corpus performs exact stdout/stderr writes
  through request-scoped bounded `IN` views, observes Linux `-ENOSYS`, and
  exits with status 42. The gate contains one personality-daemon fault,
  health-publishes generation 2, rejects generation-1 authority as stale,
  preserves an unrelated observer, reclaims the complete bounded lifecycle,
  and reaches a later timer. It does not boot Linux or Plan 9 and is not a
  distribution, root filesystem, dynamic linker, general foreign ABI,
  KVM/SVM hardware proof, PCI/device assignment, DMA/IOMMU isolation, or
  physical-device evidence.
- Mode 26 adds a bounded live Linux-to-9P2000 service composition gate in
  `ocore/kernel/smoke-live-linux-plan9-qemu.sh`. The exact Mode 25 Linux ELF,
  an unprivileged native 9P2000 server, a native supervisor, and an independently
  linked native Plan-9-style client are rebuilt into a 92,872-byte immutable
  OVFS image and loaded into isolated CPL3 address spaces. The Linux stdout and
  stderr results are read at `/srv/linux/status` through exact bounded 9P2000
  version, attach, walk, open, read, and clunk exchanges across one contained
  server fault, namespace withdrawal, health-gated generation-2 replacement,
  stale generation-1 denial, complete reclamation, and a later timer. This is
  one exact native client/server corpus, not Linux or Plan 9 boot, a Plan 9
  binary, general Linux ABI, general 9P or namespace environment, guest agent,
  KVM/SVM hardware proof, PCI/device assignment, DMA/IOMMU isolation, or
  physical-device evidence. Generation 2 is a replacement instance of the same
  server implementation and serves a later, different 20-byte snapshot after
  generation 1's read and clunk complete. Mode 26 therefore does not establish
  two independently admitted providers for one immutable object,
  requester-local route selection, recovery of one logical read on a second
  provider, fresh provider-B session/fid reconstruction, causal multi-attempt
  tracing, or live `OWRECEIPT` emission.
- Mode 31 adds the bounded M7B-1 local mechanism gate in
  `ocore/kernel/smoke-m7b-logical-read-qemu.sh`. One deterministic provider ELF
  is instantiated as two distinct generation-bound CPL3 provider principals;
  a requester-local client/router and unrelated witness instantiate a second
  ELF. Distinct A/B identities, service bindings, endpoints, and call
  capabilities are admitted before the request for the same immutable,
  content-addressed 20-byte object. A completes fresh 9P setup, returns a valid
  `Rerror`, and faults; O-core withdraws A's local route and authority, the
  client proves its retained A handle stale, and only then does B complete a
  fresh provider-local setup/read/clunk with different fids and the pinned
  digest. A volatile causal state, separate A physical/process cleanup, B
  session/queue cleanup, full bounded reclamation, witness survival, and a
  later timer all pass under QEMU TCG.
  This is M7B-1, not complete M7B. The client and router are one principal, the
  two providers share one implementation artifact, and the route set is fixed
  local configuration. The causal state is non-persisted and unsigned, not a
  live `OWRECEIPT`; the offline Mode 30 corpus is not live routing evidence.
  The detailed milestone boundary and remaining work are in
  [`ODOMAIN_PLAN.md`](ODOMAIN_PLAN.md#m7b-two-provider-immutable-9p-read-fallback).
  Mode 31 does not establish implementation diversity, general 9P/WorldFS,
  writes, fid migration, exactly-once effects, networking, Governor consensus,
  G7/G8, foreign-kernel boot, KVM/SVM, physical devices, DMA/IOMMU isolation,
  or physical-hardware evidence.
- Mode 27 adds the shared World identity PR2 gate in
  `ocore/kernel/smoke-world-identity-qemu.sh`. All 20 identity atoms named by
  the constitution have typed Rust and `.oc` definitions. A strict bounded
  `OWIDENT` v1 identity-only record converges byte-for-byte between the Rust
  oracle and native O-core under QEMU TCG. Strict decoding rejects malformed
  records and zero generation/version/term/index fields; separate hierarchical
  current/reference checks reject stale generations and same-generation
  logical mismatches. A serialized `CapabilityId` is descriptive data,
  not bearer authority, a CSpace handle, or delegation. `OWIDENT` remains the
  identity-only nested format rather than a transport, OValue envelope, or
  receipt codec; it implements no Governor or consensus and passes no G0--G13
  gate.
  Native `.oc` nominal wrappers are validated by their initializers and by
  record encode/decode boundaries; directly constructed raw aggregates do not
  bypass those checks and are not accepted records.
- Mode 28 adds the bounded canonical World wire-codec PR3 gate in
  `ocore/kernel/smoke-world-protocol-qemu.sh`. `OWPROTO` v1 has deterministic
  big-endian framing, four fixed record kinds, a 16 KiB hard maximum,
  caller/negotiated record limits, and strict rejection of truncated,
  overlong, mistagged, unknown-kind, unsupported-schema, nonzero-reserved, and
  noncanonical nested-identity records. The fixed 20-record, 1254-byte corpus
  contains two offers, one canonical v1 selection, one disjoint rejection, and
  all 16 `OWIDENT` conformance records and is byte-identical between Rust and
  native `.oc` under QEMU TCG. Its pure bounded negotiation function selects the
  highest common version and smaller record limit or one exact contextual
  no-overlap rejection; it rejects downgrade, inflated-limit, and
  false-rejection results.
  `OWPROTO` is not a stream or network transport, live handshake, authenticated
  session, encryption, replay protection, membership protocol, or authority
  channel. Identity and capability descriptions remain inert metadata: decode and negotiation grant no bearer,
  CSpace handle, delegation, or ambient identity. This slice does not implement
  PR4 OValues, PR5 receipts, a Governor,
  consensus, WorldFS, Workstream A acceptance, or any G0--G13 passage. QEMU TCG
  is not physical or hardware-isolation evidence.
- Mode 29 adds the bounded canonical World-value PR4 gate in
  `ocore/kernel/smoke-world-value-qemu.sh`. Its separate self-framed `OWVALUE`
  v1 format has a 4096-byte record maximum, depth-16 and 128-node limits, and an
  explicit portable allowlist. Records use strictly ordered fields, maps admit
  only scalar keys in canonical encoded order, and a root-only inert versioned
  extension carries one recursively portable payload. The fixed 19-record,
  928-byte corpus is 1856 lowercase hex digits with concatenated SHA-256
  `264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc` and is
  byte-identical between Rust and native `.oc` under QEMU TCG. Canonical
  decode/reencode is stable, and both sides compute the same SHA-256 over each complete record.
  Strict decoding rejects malformed, over-limit, duplicate,
  out-of-order, or otherwise noncanonical values. The hosted conversion is an
  explicit allowlist and rejects capabilities, capsules, live references,
  requests, and other authority-bearing or effectful values.
  `OWVALUE` is an offline codec and hash oracle, not a new `OWPROTO` v1 kind,
  stream or network transport, live M9 crossing, authenticated authority path,
  extension-dispatch mechanism, or execution result. Code and object references
  remain descriptive, and versioned extensions neither run code nor resolve
  authority. Mode 29 does not make the full hosted `OValue` enum portable or
  replace its canonical-CBOR shim wire format. It implements neither PR5
  receipts nor Workstream A acceptance, supplies no Governor, consensus, or
  WorldFS, and passes no G0--G13 gate. QEMU TCG is not physical or
  hardware-isolation evidence.
- Mode 30 adds the bounded canonical World-receipt PR5 gate in
  `ocore/kernel/smoke-world-receipt-qemu.sh`. Its separate self-framed
  `OWRECEIPT` v1 record binds bounded descriptive World identities and
  generations, SHA-256 content references, capability-right descriptions,
  terminal and commit fields, evidence-gate identity, and an algorithm-tagged
  signature envelope. Rust and native `.oc` produce the same fixed two-record,
  3,239-byte corpus (6,478 lowercase hex digits; SHA-256
  `1edd90bf881cd42d08e2031482baae4e7c9a95bd78cfa65f0cbe14147c0a2604`) and
  the same 1,575-byte current and 1,546-byte stale signing preimages. Both
  strictly reject malformed,
  over-limit, reserved, out-of-order, or otherwise noncanonical records.
  Hosted Rust uses a pinned, explicitly non-secret conformance key for real
  Ed25519 sign/verify, tamper, and wrong-key tests. Native Mode 30 structurally
  validates the signature envelope but does not implement or prove a general
  freestanding Ed25519 verifier.
  Receipt capability identities and rights remain descriptive data, not
  bearers, CSpace handles, delegation certificates, session tokens, or grants
  of authority. Signature validity does not establish authorization, trusted
  signer policy, or current World state. The Mode 30 corpus is constructed
  offline and is not evidence that another subsystem emitted a receipt. This
  slice supplies no production key generation or custody, enrollment, rotation,
  revocation, transport, authoritative replay/commit fencing, Governor,
  consensus, WorldFS, typed World Alpha attestation, Workstream A acceptance,
  or G0--G13 passage. QEMU TCG is not physical or hardware-isolation evidence.
- The separate World-project hosted-reference slice consumes an exact
  snapshot-derived `DeploymentPlanV1` through
  `ProjectCoordinator::new_world_bound`. Before schedule derivation, workspace
  materialization, or child-process launch, it re-derives the trusted logical,
  deployment, and snapshot records and fences caller-supplied current
  World/Governor identity; a dedicated coordinator observer
  node/domain/optional-process; a dedicated coordinator attempt; selected
  provider node/domain/optional-process/service generations and implementation
  digest; and every logical operation's exact task attempt. The coordinator
  attempt must use a task distinct from every operation attempt and becomes the
  trace execution-attempt identity. The launch profile is explicitly
  non-authorizing: these caller assertions are not authenticated membership,
  proof that the host owns the observer identity, Governor admission,
  capability or lease authority, provider reservation, or remote dispatch.
  A terminal `RuntimeGraphV1` is admitted only after plan-aware causal replay
  against the trusted `ProjectHGraph` and exact deployment. It binds the exact
  logical/deployment/launch/snapshot schemas and digests,
  World/observer/coordinator-attempt/provider/task-attempt context, normalized
  trace ordinals and outcomes, and per-operation residual `HostWorld`.
  Never-started operations have empty observations. Its neutral
  `RouteSettlement` terminal covers success, nonzero settlement, and guard skip;
  terminal residual `HostWorld` is aggregated across every actually observed
  started or terminal operation rather than copied from only the selected route.
  `execute_world_project_with_receipt` then uses a caller-supplied Ed25519 signer
  to emit canonical OWRECEIPT v1 with
  `ReceiptCommitFenceV1::Uncommitted`. The receipt context places the dedicated
  coordinator observer and attempt, not the proposed provider or a route
  attempt. Its subject leaves package absent instead of overloading that field
  with the provider implementation. Only route success yields receipt success;
  nonzero and guard-skipped settlements yield receipt failures. Signature
  integrity is neither Governor authority nor a governed commit.
- Native Mode 32 consumes that caller-generated receipt as one bounded canonical
  lowercase-hex record. It performs full canonical decode, exact re-encoding,
  validated signing-preimage construction, requires the uncommitted fence, and
  compares a domain-separated SHA-256 over the complete unsigned canonical body
  with the hosted value. It then reuses the successful validation scratch with
  a malformed envelope and requires the old terminal/commit tags to be
  unavailable. The required no-argument end-to-end gate generates the hosted
  vector; the second command is the direct two-argument vector interface:

  ```bash
  ./ocore/kernel/smoke-world-project-runtime-qemu.sh
  ./ocore/kernel/smoke-world-project-receipt-qemu.sh RECEIPT_HEX_FILE EXPECTED_SEMANTIC_SHA256
  ```

  Mode 32 does not execute the project or verify Ed25519 natively. The complete
  hosted-reference slice supplies no Governor admission/commit,
  capability/lease issuance, reservation, remote dispatch, recovery, or
  exactly-once protocol. QEMU TCG is not physical hardware or hardware-isolation
  evidence, and this passes neither G1 nor Workstream A acceptance.

## Ostadix World native Alpha boundary

- [`OSTADIX_WORLD.md`](OSTADIX_WORLD.md) is the normative native constitution.
  It fixes the replicated-Governor model, OValue/capability/capsule crossings,
  explicit aggregate-memory model, fifteen workstreams, and G0--G13
  convergence ladder. Defining that target is not evidence that a gate passed.
- [`world_alpha_gates.toml`](../evidence/world_alpha_gates.toml) defines 14
  entries--the G0 constitutional baseline plus 13 integration gates through
  G13--with their dependencies, qualifying evidence classes, and prohibited
  substitutes. [`world_contract_v2.toml`](../evidence/world_contract_v2.toml)
  composes the byte-frozen
  [`world_contract_v1.toml`](../evidence/world_contract_v1.toml) vocabulary and
  the separately versioned O-Machine schema. The frozen import retains the
  three crossing kinds, all 20 identity atoms, seven failure classes, eight
  consistency rules, and evidence-class taxonomy without rewriting historical
  attestations. `scripts/world_alpha_evidence.py` validates the composition and
  its exact Rust/native/document references. Schema v4 keeps the registry
  definition-only, resolves active append-only evidence heads, derives claims
  from typed observations, and computes status. Derivation identity is recorded
  separately from the claim cache: a policy change requires an immutable
  `rederive` event with the exact claims lost and gained, rather than rewriting
  or invalidating the old attestation. Only G0 and G2 currently derive `passed`;
  G13 and the other 11 gates remain `defined`.
- [`O_MACHINE_CONTRACT.md`](O_MACHINE_CONTRACT.md) settles the future G7/G8
  machine boundary as resource-class-specific and asynchronous. G7 gives no
  machine handle or Ostadix HVC to the guest and carries no handle MAC/key
  lifecycle. `MachineMemory` revocation is terminal teardown; a graceful guest
  error must instead come from a device-native path such as virtio-blk `EIO` or
  a negotiated 9P error. This contract is not evidence that stage-2 teardown,
  TLB shootdown, those device errors, G7, or G8 are implemented.
- [`LOCAL_AUTHORITY_ROUTING_AMENDMENT.md`](LOCAL_AUTHORITY_ROUTING_AMENDMENT.md)
  records a proposed additive split between one local O-Machine authority per
  physical node, node-local O-core policy, requester-local route selection, and
  replicated Governor facts. It also separates local route exclusion, lease
  expiry, owner revocation, and physical reclamation. It does not alter the
  sealed contracts or establish the complete amendment as implemented or
  passed; those claims require versioned successor schemas and executable
  evidence. Mode 31 is only the bounded M7B-1 local mechanism described above.
- G2 is one forced-QEMU-TCG, one-vCPU AArch64
  `virt,virtualization=on` run. It compiles native semantic `.oc` code to
  `EM_AARCH64`, installs a resident EL2 vector and stack, enters O-core at host
  EL1, and completes one domain-separated HVC/ERET round trip with checked
  register and stack integrity. The HVC is a host-EL1 probe, not a guest
  hypercall interface. It then enters two EL0 principals through a
  real exception-return path, handles their SVC calls at EL1, exercises endpoint
  request/reply and attenuated capability transfer, contains one EL0 fault,
  tears down and reclaims generation-tagged state, rejects stale use after slot
  reuse, and establishes bounded post-lifecycle architectural-counter progress.
  Assembly is limited to boot/vector/context/EL2 enforcement glue and is checked
  not to contain the semantic PASS strings.
- G2 is not physical AArch64 or KVM/SVM evidence; it is single-core and does
  not pass G3. It does not install stage-2 translation, prove timer-interrupt
  delivery, boot Linux or Plan 9, provide a general foreign ABI,
  assign a PCI or physical device, isolate DMA through an IOMMU/SMMU, or pass
  any later native World gate.
- [`HOSTED_WORLD_REFERENCE_PROFILE.md`](HOSTED_WORLD_REFERENCE_PROFILE.md)
  retains the prior hosted design only as a simulator, differential oracle,
  fuzzer, and development console. A hosted deployment cannot satisfy a native
  release gate. The narrower hosted Live-World package/service oracle remains
  separately bounded.
- The existing project runtime, HGraph executor, capability broker,
  KernelWorld lifecycle, and Modes 20--30 are reusable organs, not an integrated
  World. Mode 23's synthetic guest is not G7 or G8; Mode 25's static ELF is not
  G9; Mode 26's exact 9P2000 corpus is not G6; supplemental Mode 21 is not real
  Linux boot or physical-device isolation.
- No replicated Governor, native membership transport, WorldFS, physical
  multinode HGraph execution, real Linux KernelWorld boot, physical-device
  assignment, DMA/IOMMU isolation, native Debian personality, or accelerator
  fabric is claimed. The shared Rust/`.oc` identity vocabulary and hosted
  identity/effect vocabulary are still foundations: descriptive names,
  inventory, snapshots, and serialized identities remain non-authority.
- The grounding command labels a report with a caller-supplied World identity.
  It does not read a current World snapshot, enforce freshness, or bind
  execution. Hosted ResourceKey PR6 now supplies typed World, Governor, node,
  domain, process, generic resource, object, descriptive capability, namespace,
  task-attempt, artifact-publication, device, and accelerator state classes.
  Device and accelerator views share the generic resource dependency, and
  source effect declarations cannot mint governed state or authority. No
  production lowering emits governed `ResourceKey` effects yet; ordinary hosted
  operations retain their conservative `HostWorld` dependency.
- `scripts/smoke-world-resource-keys.sh` is bounded hosted
  repository-conformance evidence for vocabulary, underlying identity helpers'
  caller-pair comparison, HGraph state chaining, alias-aware grounding
  projection, source-forgery rejection, and residual `HostWorld`. Grounding
  checks only the bound World epoch/membership, not authoritative nested
  freshness. This is not O-core Mode 31, a cross-language wire format,
  native/QEMU or hardware evidence, Governor authority, device assignment,
  DMA/IOMMU isolation, Acceptance gate A, or passage of G0, G1, or any G0--G13
  gate.
- PR7 now provides a bounded hosted project logical planner. A directory or
  lifted `ProjectBundle` is resolved through the shared typed route selector,
  bound by an exact bundle digest and policy in `ProjectExecutionPlan`, and
  projected into a validated HGraph containing real `MaterializeProject`,
  `BuildRoute`, `RunRoute`, `SelectRoute`, and, for `verify_equivalent`,
  `CompareRouteResults` operations. Alternative materialization branches and
  prerequisites are explicit in both layers. The project plan also records
  guards, environment overlay key names, ambient environment-guard
  dependencies, inputs/outputs, declared effects, cancellation, and
  equivalence policy, including each route's `failure-continuation` contract;
  its HGraph projection carries the corresponding
  operation, effect/resource-transition, dependency, and output topology.
  Project-bundle format v2 carries that safety contract. The v1 reader path
  migrates only bundles in which every route omits the v2 field, defaults those
  routes to `unproven`, and emits v2 thereafter; a v1 document carrying the new
  field and an in-memory bundle mislabeled as v1 are both rejected.
  Planning is deterministic and nonexecuting, and exact source/projection
  validation rejects malformed references, substitution, and graph forgery.
- World PR8-1 adds the bounded hosted project-profile `LogicalHGraphV1` schema.
  It canonically records the exact bundle/selection binding, operations, typed
  dependencies, route facts, declared input/output paths, raw effects, and
  scheduler-expanded resources while retaining residual `HostWorld` and rejecting fabricated
  governed resources or authority requirements. Its domain-separated digest is
  an exact-source-bound projection identity, not a whitespace-insensitive
  source-semantic hash: source bytes, file modes, and manifest formatting alter
  the bundle and therefore the graph identity; canonical decode/re-encode
  normalizes only the logical JSON record. `LogicalHGraphV1` itself does not
  supply deployment, runtime, recovery, World task identity, receipts, native
  parity, or G1 evidence.
- World PR8-2 adds bounded canonical `PlacementSnapshotV1` and
  `DeploymentPlanV1` intention records. The exact hosted-unbound plan maps
  operations for supported coordinator policies only to `AmbientHost` or
  `HostedCoordinator`, carries no World/task/provider identity, and keeps
  unsupported hosted policies `Unresolved`. Its active derived requirements
  are the exact bundle plus bundle-scoped role/path declarations, runtime
  classes, executable/evaluator facts, platform and ambient-environment guards,
  authority absence, and residual `HostWorld` admission. Bundle environment
  overlays are recorded separately. Architecture, package, and failure-domain
  fields are schema vocabulary but are currently unconstrained or empty.
- A snapshot-derived `ProposedProvider` requires a caller-supplied exact
  World-epoch `PlacementSnapshotV1` and one caller-supplied exact
  `TaskIdentity` per logical operation. Canonical provider selection is a
  deterministic descriptive proposal, not authenticated or current inventory,
  Governor admission, authority, dispatch, reservation, health, or execution.
  `require_current_world` checks only World identity/epoch. The ordinary opt-in
  executor does not consume this plan, but the
  explicit hosted-reference World entry point consumes it together with
  `HostedWorldLaunchV1` and a caller-supplied current view. The launch contains
  a coordinator observer and a coordinator attempt distinct from the proposed
  provider and every per-operation attempt. That bounded path produces a
  causally replayed terminal `RuntimeGraphV1`, a caller-signed uncommitted
  OWRECEIPT whose placement is the observer and whose package subject is absent,
  and Mode 32 native canonical/semantic comparison as described above. It does
  not implement
  authenticated placement, a `RecoveryPlan`, Governor admission/commit,
  capability or lease authority, reservation, remote dispatch, recovery,
  exactly-once execution, native project execution, native Ed25519 verification,
  physical-hardware evidence, G1, or Workstream A acceptance; G1 remains defined
  and unpassed.
- `scripts/smoke-project-hgraph.sh` is the composite Project HGraph hosted
  planning and generated-adapter gate. Its PR7 phase proves repository-owned
  `scripts/o-cli.sh plan` parity and deterministic, nonexecuting IR/DOT. Its
  PR8-1 phase exercises canonical/strict logical encoding and digesting,
  scheduler-expanded resources, `HostWorld` retention, governed-resource and
  authority forgery rejection, and trusted project-projection comparison. Its
  PR8-2 phase exercises canonical hosted-unbound and snapshot-derived
  deployment records, exact logical/bundle binding, bundle-scoped role/path
  compatibility, deterministic provider proposals, World/task hierarchy
  rejection, and trusted substitution rejection. It
  then compiles a project binary, checks route listing and option/policy
  rejection, and runs opt-in AnySuccess for immediate short-circuit plus
  explicitly admitted nonzero-to-success continuation in disposable
  workspaces. The continuation fixture declares its executed prerequisite and
  failed first route `failure_continuation = "declared_idempotent"`; omission
  defaults to `unproven` and stops before the second branch. Route
  materialization and commands retain
  conservative fallible `HostWorld` effects even when a manifest declares
  `pure=true`. Logical alternative branches may therefore be serialized and
  cross-coupled by the shared ambient/resource state chains; PR7 does not prove
  parallel branch execution or independent host mediation.
- `scripts/smoke-project-hgraph-exec.sh` is the opt-in, ordered-alternative
  hosted execution gate. Under `O_PROJECT_EXECUTOR=hgraph`, a validated Project
  HGraph controls isolated materialization, `Value` versus `Success`
  prerequisite readiness, and policy selection for `Explicit`, `Default`, and
  serial ordered `Fallback`/`AnySuccess`. Only the two first-success selectors
  derive `ReadyInputPolicy::OrderedFirstSuccess`; every other operation retains
  conjunctive input readiness. Fallback follows resolved priority order,
  AnySuccess follows declaration order, and a first success prevents later
  branches from materializing or starting. Every attempted alternative result
  is retained. When the terminal alternative settles unsuccessfully, it admits
  the next route only when every route child was guard-skipped or every route
  that executed in that branch, including successful prerequisites, carries
  the bundle-bound `failure_continuation = "declared_idempotent"` contract. The default
  `unproven` class denies continuation; an infrastructure abort publishes no
  route value and stops the policy. A nonzero prerequisite publishes its value
  and conservative `HostWorld` successor but withholds completion; because no
  first-class branch-failure value is synthesized in this slice, a failed
  prerequisite hard-stops even when it declares idempotence. The unsigned
  diagnostic trace v5 binds a canonical `LogicalHGraphV1` schema/digest plus
  stable source identity and the exact canonical hosted-unbound
  `DeploymentPlanV1` schema/digest to a fresh execution-attempt identifier and
  distinguishes settlement, guard skip, and abort. Its continuation decision
  records the assessed route prefix, proposed next route,
  `no_execution`/`declared_idempotent`/`unproven_effects` evidence, and the
  allow/deny result; a denied decision is persisted before the CLI reports no
  successful route. Standalone replay checks only structural lifecycle
  consistency. Plan-aware replay against a trusted `ProjectHGraph` verifies the
  header/projection, reconstructs the hosted-unbound deployment artifact and
  rejects its substitution, verifies exact operation identities and next
  alternative, requires complete causally ordered lifecycle coverage for every
  transitive route prerequisite, recomputes evidence from `RoutePlanFacts`, and
  rejects missing decisions or later-branch events after denial; every complete
  coordinator trace passes that semantic replay before return. Operations never
  attempted after short-circuit or denial emit no lifecycle event.
  Parallel/racing, aggregate, equivalence, and benchmark policies fail closed
  rather than falling back to the compatibility runtime. The execution-attempt
  identifier is diagnostic and is not a World `TaskIdentity` or
  `TaskAttemptIdentity`.
  `declared_idempotent` is an author declaration, not independently verified
  idempotency, sandboxing, effect journaling, fencing, compensation, or an
  exactly-once guarantee. This rule exists only in the opt-in hosted HGraph
  coordinator and does not alter the default compatibility runtime. The
  ordinary opt-in path remains distinct from the explicit World-bound
  hosted-reference adapter. Neither proves parallel race/cancellation, retry,
  authenticated or actual remote placement, Governor admission/commit,
  capability/lease enforcement, reservation, recovery, remote dispatch,
  exactly-once effects, native project execution, native Ed25519 verification,
  physical-device assignment, PCI/DMA/IOMMU isolation, physical hardware,
  Workstream A acceptance, G1, or passage of any G0--G13 gate.
- Neither the present repository nor the Alpha target claims coherent
  cross-node RAM, transparent remote pointers, arbitrary Linux compatibility,
  universal hardware support, or transparent migration of every process.

## Implemented conservatively

- Graph construction, multi-output validation, type/fidelity solving, explicit
  resource and completion lowering, scheduling, and readiness-driven dispatch
  are implemented in `src/hgraph/`, `src/effects.rs`, and `src/executor/`.
- Strict-equivalent parallel dispatch is correctness-first: compiler-verified
  O-scope reads and verified pure, deterministic, infallible, state-free inline
  renderers run on workers. Ordinary Eval/shim operations stay on the evaluator
  owner thread and are ordered by graph state. Separately, direct attribute-free
  ephemeral shim members of a group under effective `autonomous(...)` may run
  on bounded workers with evidence-labeled
  `explicit-autonomous-unordered` semantics. This preserves explicit dataflow,
  live capability checks, and deterministic outcome selection, but does not
  claim rollback or ordering of already-started hidden host effects. The
  default pool size is bounded by both live host parallelism and the widest
  admitted local-worker static wave, with a conservative host fallback of one
  if parallelism cannot be queried. An explicit runtime `O --workers N`
  override replaces that default and is pool capacity only: it is not clamped
  to the reported host count or wave width and does not override graph
  readiness or prove that `N` tasks can execute together. Coordinator-only
  graphs create no worker pool.
- `olangc --target ir --explain-schedule` is non-executing inspection. Its
  advisory `oexec.realizability/v1` marker is outside the admission digest;
  `--workers N` there changes only the displayed capacity comparison, not any
  execution. The marker distinguishes the maximum total static-wave width from
  its local-worker subset. `worker-count-covers-static-wave=yes` means only
  that the selected count is at least that local subset; `no` means it is not,
  and `not-applicable` means there are no local-worker operations. Every case
  retains `execution-realizable=unknown`: no value proves simultaneous
  dispatch, CPU/memory fit, external-runtime readiness, placement, or observed
  overlap. Static widths are neither runtime batches nor dynamic-frontier
  bounds.
- Ordinary source sequence is preserved by completion dependencies unless an
  explicit concurrent group or the narrow verified-inline rule removes it.
- Full N-language communication soundness is not established; native OValue
  crossings remain conservatively represented as `NativeCapsule`.

## Research directions enabled by the architecture

- Parallel read leases and verified path-, endpoint-, and service-specific
  resource models that safely reduce `HostWorld` serialization.
- Runtime plugin registration beyond the current static `BackendRegistry` table
  in `src/ir.rs`.
- Fingerprint-complete effect tracking, enforced sandboxes, and verified backend
  analyzers that could broaden strict-equivalent graph parallelism beyond the
  trusted inline/read-only set.
- More precise backend morphism proofs and fidelity accounting for OValue
  crossings, extending the current `Fidelity` and `BackendMorphism` vocabulary
  in `src/value.rs`.
- Deterministic cancellation and result-selection semantics for concurrent
  groups and future graph execution.
- O-Domain evolution beyond the current bounded Mode 24--26 gates: add pinned
  windows, streaming, signals, real mapping/resource events, post-reply
  lifecycle-race evidence, fuzzing, allocation-failure coverage, and concrete
  delegated services, then broaden the exact Mode 25 Linux corpus only behind
  equally explicit ABI and lifecycle evidence. Mode 26 adds only its exact
  bounded 9P2000 client/server corpus, not a general 9P namespace. Durable
  reboot reconstruction and a capability-bounded build service also remain
  future work. No general Linux ABI or root filesystem is claimed.
  The staged engineering plan is in `docs/ODOMAIN_PLAN.md`.
