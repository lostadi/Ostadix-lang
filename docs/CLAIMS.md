# Claim-accuracy inventory

## Implemented and tested now

- Expression-granular recursive evaluator composition is implemented by typed
  expression syntax described in `README.md`, lowered from parser nodes to OIR in
  `src/ir.rs`, and executed by the Rust evaluator in `src/eval.rs`.
- The accepted evaluator tags are registry-extensible at compile time through
  the declarative catalog in `src/backend_catalog.inc.rs`. `BackendRegistry`,
  native-adapter dispatch, generated-runtime source emission, and MCP runtime
  discovery are compile-time projections of that one catalog; no runtime source
  parser or independently maintained MCP backend table is involved. The catalog
  records canonical tags, aliases, purity metadata, splice rendering, execution
  mode, adapter ownership, backend authority requirements, typed integer/rich-
  number preservation capabilities, and descriptive executable alternatives.
  Executable presence and declared value capability are not health,
  authorization, capacity, or operation admission.
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
  crate license, example/evidence manifest schemas, sealed OSTADIX Alpha
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
- `oexec.execution-intent/v1` is a process-stable, authority-free identity over
  exact source bytes, lowered OIR, plan, solved graph, the referenced canonical
  backend specifications, analyzer, and base policy. The MCP server can retain
  a bounded, expiring, one-use opaque handle and require `O` to recompute that
  same intent before dispatch. This is a local same-intent gate, not a
  capability, runtime-health result, retained admission, or authorization; `O`
  still constructs and rechecks a fresh process-local V5 admission, and direct
  `o_run` remains an explicitly ungated compatibility surface.
- Admission V5 remains the supported legacy-local contract. Hosted Placement
  V6 is an additive placement-aware contract; neither version is silently
  upgraded or translated into the other. Existing MCP execution tools remain
  local V5 surfaces. The direct V6 channel is exposed separately through
  `o-node` and `octl node ...`; it does not upgrade a V5 handle.
- The V6 placement core models a `RequirementFootprintV1` over operation
  capability, value, effect, environment, and resource constraints and compares
  it with a full `TargetDescriptorV1`. Eligibility never factors through an ISA
  or language display name. Complete, conservatively unknown, and unsatisfiable
  footprints remain distinct, and unknown requirements cannot join away.
- `o why FILE.O P<N>` appends the compiler-derived V6 footprint for the exact
  selected plan node. Hosted shim effects without explicit autonomous consent,
  coordinator-local control, and unpackaged scope state remain
  `ConservativeUnknown`; the report is descriptive and grants no placement
  authority or lease.
- Every positive V6 placement decision carries exact requirement-to-warrant
  discharge. Compiler-static requirements and fresh runtime-discovered or
  enforced target facts are the strict default. Provider declarations and
  historical observations may authorize missing positive facts only under an
  explicit trust policy, cannot override a fresh discovered negative, and are
  bound into the transport-independent discharge record. Frozen direct-node V1
  does not consume that proof. Durable hosted V2 carries the complete profile,
  capacity observation, requirement footprint, warrants, discharge, trust
  policy, and compute reservation under one signed envelope; the node
  re-evaluates candidate eligibility against its current catalog and exact
  locally prepared fragment before accepting execution authority.
- Admission V5/V6 and backend-catalog generation are independent version axes.
  The current authorizing catalog is `ostadix.backend-catalog/v4`, and its
  schema string participates in the whole-catalog and per-specification hash
  domains. `NodeProfileV1::validate_at` invokes
  `TargetDescriptorV1::validate_current_backend_catalog` before candidate
  authorization. A profile containing a V3 or otherwise unknown backend
  specification therefore fails with `NonCurrentBackendCatalog`, even if its
  old digest, detached signature, requirements, and warrants agree with one
  another. Decoding or independently verifying an archived signed record is
  not current placement authorization, and no V3 digest is relabeled as V4.
  V4 additionally binds the explicit backend-state support and snapshot-
  compatibility declaration used by persistent-session placement.
  A descriptor with no backend implementations may remain structurally valid,
  but cannot discharge a backend-specification or backend-implementation
  requirement.
- Current backend implementation identity uses the path-independent
  `ostadix/backend-executable-set/v2` projection and
  `ostadix.local-realization/v2` hash material. The semantic executable set
  binds the selected catalog alternative, selection kind, logical command,
  executable role, and immutable artifact bytes; physical paths and retained
  file handles stay in process-local admission authority. Current profile
  validation reconstructs the realization through `BackendRegistry`, rejects a
  foreign protocol ABI, and rejects legacy local-realization V1 material with
  `NonCurrentBackendImplementation`. Archival V1 bytes remain inspectable and
  are never relabeled as V2.
- Registry v1 is a transport-independent, canonical-CBOR, Ed25519-signed
  append-only store for namespace-scoped `placement::NodeProfileV1` records.
  It verifies pinned roots, strict-descendant namespace delegation, sequence
  and previous-event chains, rejects future-dated events, contains each profile
  validity interval within one signer authority, checks profile freshness and
  monotonic generation, and rejects rollback, forks, equivocation, or untrusted
  imports before atomic replacement. Cooperating publish/import writers hold a
  persistent sibling advisory lock for the complete transaction. `o-registry`
  provides local `init`, `profile-local`,
  `publish-profile`, `verify`, `list`, `export`, and `import` operations; it is
  not a network daemon, discovery service, health oracle, lease issuer, or
  execution authority, and the direct node path does not consume it.
- HGraph represents ordinary results, successful completion, evaluator state,
  host-resource state, and persistent actor state as nodes. Executable
  operations are directed, multi-output hyperedges. Readiness follows only from
  materialized inputs and their producers.
- Unknown hosted operations are conservatively serialized through a shared
  `HostWorld` state chain. Persistent environments also use typed actor-state
  chains. The implementation does not claim exact effect inference from
  arbitrary Python, Bash, JavaScript, Rust, or other hosted source.
- The current V5 scheduler's `ActorResourceId` remains the canonical backend
  name plus persistent numeric environment; the process registry additionally
  keys sandbox and admitted launch generation. Hosted V2 does not substitute
  that smaller local scheduling key. It uses `ActorGenerationIdV1` as its
  physical state coordinate, binding the logical environment, exact backend
  implementation, target descriptor, sandbox policy, launch context, and
  generation. `StateSessionIdV2` is the separate logical durable-session
  identity, so a replacement generation cannot alias the actor it replaced.
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
- Literal `o-link` wrappers use linker-isolated `[*]` environments rather than
  synthesized persistent numeric indices. Authored numeric environments remain
  persistent logical affinity. Sequential `LANG[*]` syntax and fresh semantics
  are implemented by Rust, the Python reference, and C17. Ordered `LANG[*]`
  wrapping remains the cross-edition form;
  `o-link --parallel` is explicit autonomous consent, each admitted parallel
  run returns input-ordered results, and sequential structural/inlined `.O`
  boundaries split those runs. Its emitted `autonomous(batch(...))` call
  expression currently requires the Rust edition: C17 schedules serially and
  the Python reference lacks call-expression grammar. Parallel linking does not
  by itself establish remote eligibility or rollback already-started hidden
  effects. Detected import/include dependencies form topological barrier waves;
  only same-wave antichains overlap, and dependency cycles are serialized in
  stable source order.
- The bounded direct-node transport uses synchronous TCP with TLS 1.3-only
  mutual X.509 authentication, a pinned CA and server name, required client
  certificate/key, and version-specific ALPN. It has no plaintext, 0-RTT, or
  post-negotiation downgrade path. Canonical-CBOR frames are limited to 2 MiB;
  operation source to 1 MiB; result payload to 768 KiB; connect/handshake to 10
  seconds; and I/O to 60 seconds by default.
  The embedding `o-node` process is not treated as the O backend proxy: doctor
  and serve resolve a native `ostadix-evaluator`/sibling O or an explicit
  `--runtime-binary`, reject script dispatchers, and bind that exact executable
  into each operation's retained V5 executable manifest. Doctor's native-image
  check is a format preflight, not an ABI or backend-protocol probe; the
  default/sibling O path is exercised end-to-end, while an arbitrary explicit
  image proves protocol compatibility only on an admitted hosted-backend
  launch. Registry
  `profile-local` records default to 45 seconds and accept integer lifetimes
  from 1 through 60 seconds.
- Frozen transport V1, exposed as `octl node run`, sends one operator-selected node a
  `RemotePreparedOperationV1` binding exact source SHA-256, task/attempt
  identities, the full descriptive backend-catalog digest, deadline, and output
  ceiling. The node creates a fresh evaluator for the operation; it exposes no
  generic shell-command RPC, project-bundle dispatch, or persistent remote
  actor. It returns a canonical-CBOR, SHA-256 self-digested
  `HostedOperationReceiptV1`. The deadline suppresses a late result but cannot
  cancel evaluator effects that were already running.
- V1 compares the exact whole-catalog digest and therefore
  rejects peers built from different catalog generations. That is a protocol-
  compatibility binding only: V1 still does not consume a placement
  profile, warrant discharge, or lease.
- Durable transport V2 is an explicit, non-upgraded ALPN path. Open and recover
  commands require a signed `StateControlLeaseV2`; execute requires a signed,
  one-use `PlacementLeaseV2`. `SignedPlacementLeaseV2` authenticates the
  canonical authority, exact hosted command, full placement evidence, and the
  open-session state-capacity observation when present. The current node pins
  one Ed25519 placement-authority key and requires the profile, capacity,
  warrants, and state-capacity record to name that issuer. This is a bounded
  single-issuer adapter, not production enrollment, rotation, revocation, a
  multi-key chain, discovery, or scheduler-selected placement.
- Before V2 execution authorization, the node parses, lowers, solves, admits,
  and seals the exact submitted source as one non-cloneable
  `PreparedPlacementFragmentV1`. The admissible shape has one non-whitespace
  semantic root and exactly one shim `Exec`; text children are allowed, while a second `Exec`, `Load`,
  `Store`, `Call`, `Request`, `Group`, `Schedule`, text-only input, a nonempty
  coordinator scope, or recursive `O.eval` authority is refused. The execution
  lease binds the resulting OIR, footprint, portable placement admission, task attempt, backend
  implementation, realization pipeline, trust policy, compute reservation,
  state session, and the applicable actor generation. Persistent opaque shim work requires
  the explicit `execution/session-serialized-opaque-effects@1` target
  capability; this supplies per-session serialization, not purity,
  replayability, or global effect isolation.
- Session open fixes `HostedPlacementIdentityV2`: target, exact requirement
  footprint, backend implementation and realization pipeline, logical
  environment, trust policy, and compute reservation. Open has no physical
  actor generation. A stateful first execute carries no pre-established actor;
  after exact local preparation the node derives and signs
  `ActorGenerationIdV1`, including exact sandbox and launch context, and later
  executes must match it. Open does not freeze one source/OIR for the complete
  session lifetime: every execute separately binds exact source-derived OIR,
  task attempt, portable placement admission, deadline, operation, and one-use
  lease, permitting multiple commands only while those fixed coordinates remain
  equal. The full V5 admission still binds process-local runtime freshness and
  is rechecked at dispatch; it is not used as a cross-process proof coordinate.
- V2 session access requires both the authenticated TLS client-certificate
  leaf fingerprint fixed at open and a separate random 256-bit bearer. The
  client creates and fsyncs its mode-0600 capability file before network send,
  and the signed Open request commits to the exact capability. The durable store
  keeps only its commitment, a random salt, and a salted hash. A durably
  committed Open whose response was lost can be recovered by resending the
  byte-identical full signed request and capability, including after restart or
  proof expiry; conflicting request bytes are rejected. Session
  directories/files are owner-only on Unix,
  symlinks and non-regular files are rejected at trust boundaries, and journal,
  operation, checkpoint, and directory updates are synchronized before
  acknowledgement. A new session plus its first receipt is atomically published,
  and immutable operation/checkpoint blobs use private same-filesystem staging
  plus no-clobber publication. Source, results, and checkpoint material are not
  encrypted at rest.
- Every V2 mutation is represented in a node-Ed25519-signed, hash-chained
  canonical journal. Exact duplicate client sequence/request/digest triples
  return their prior commit receipt; conflicting reuse is rejected. Startup
  verifies session and authority journals, reconstructs accepted and refused
  placement-lease nonces, and reconstructs the terminal record's signed
  `state_durable` and `actor_state_touched` disposition without guessing. An
  accepted operation whose backend command never started remains `NotStarted`,
  while its allocated physical generation is either retired when state is empty
  or fenced as lost when prior state existed. Started-without-terminal work is
  `Ambiguous`. These records support reconnect status and replay detection; they
  do not establish exactly-once execution or external-effect publication.
- Explicit closed-session GC retains the complete signed terminal session
  journal by a same-filesystem atomic rename into the permanent tombstone
  archive. The signed GC authorization binds its exact raw digest, byte length,
  and terminal head, so retired session identity and every consumed lease nonce
  survive payload deletion and restart. The retained journal is excluded from
  reclaimed-byte claims. A fixed 16 KiB authority-control debit funds signed
  tail-repair and GC frames without exceeding the hard total-state quota;
  completed GC credits only its verified reclaimed bytes. Reclaiming cycles can
  therefore recycle the reserve, while a zero-reclaim history can exhaust it
  and is refused rather than evicting state.
- Under exclusive store ownership, each journal is signature/hash-chain scanned
  once at startup and subsequent appends advance a cached exact head; an
  out-of-band length change is rejected. Startup truncates only an incomplete
  final frame and appends signed repair evidence to the authority journal; a
  complete invalid frame is never repaired. There remains a narrow crash window
  after the truncation fsync and before that audit append in which the retained
  prefix is sound but the repair event can be absent. If a filesystem barrier
  cannot be reconciled to exact durable bytes, the store returns
  `store-reopen-required` and refuses mutations plus current-head views until a
  fresh open revalidates the journals.
- `Status` and `Actors` responses verify a node-signed receipt for the exact
  session journal head and correlate it to the requested session. Their
  projected convenience fields are carried over authenticated mTLS but are not
  individually covered by that receipt; callers requiring offline proof must
  consume the signed journal itself.
- `StateQuotaLimitsV2` has five canonical hard dimensions: open sessions,
  actors per session, snapshot bytes per actor, state bytes per session, and
  total state bytes. Open carries a fresh signed state-capacity observation and
  exact reservation. Exhaustion refuses new state or work and never retires an
  existing actor to make room; this initial runtime realizes exactly one actor
  per session. Closing releases the reservation and stops the actor but retains
  journal files. Only the explicit offline `o-node admin gc-closed` path, under
  the cooperating exclusive advisory state-root lock, removes a durably closed
  session after writing signed GC-authorized and GC-completed authority-journal
  records. That lock coordinates the shipped node/admin processes; filesystem
  ownership is not a hostile-same-UID security boundary.
- V2 exposes four state-tier labels but authorizes only three mappings:
  `Stateless` requires a fresh fragment and current `Stateless` catalog support;
  `CheckpointRestore` requires a persistent environment and current
  `SemanticSnapshot` support with its exact codec/compatibility identity; and
  `LiveActorOnly` requires current `ExternalPinned` support and remains tied to
  the node process. `ReplayReconstructible` is rejected because no current
  catalog tier or automatic replay/publication adapter discharges it. Restart
  validates an eligible checkpoint, durably fences the lost physical
  generation, and enters `RecoveryRequired`; it does not lazily restore during a
  user operation. Authenticated Python/SQL recovery writes a signed
  `RecoveryAttemptStarted` before replacement launch, forcing a unique actor
  generation and nonce consumption, then publishes `RecoveryCommitted` only
  after the backend acknowledges the exact staged snapshot. Startup converts an
  unterminated recovery attempt into a signed refusal before exposing the
  session. Lost live-only state remains `RecoveryRequired`, and unreviewed
  codecs fail closed.
- V2 checks an absolute deadline before admission, before evaluator entry, and
  in the prepared evaluator wait. Expiry before
  dispatch has a typed no-command-sent result. In-flight timeout or process
  loss can remain ambiguous: the runtime cannot cancel, compensate, or roll
  back external effects already performed, and a late value is suppressed.
  Output encoding, actor checkpointing, journal fsync, and terminal response
  publication occur after value acceptance and are not covered by an end-to-end
  publication-deadline claim.
- V1's self-digested `HostedOperationReceiptV1` has no detached node signature:
  it is tamper-evident after capture but not independently attributable or
  offline-verifiable. V2 responses instead carry node-signed journal receipts
  checked against an explicitly pinned node receipt key. Neither path performs
  automatic registry discovery, target selection, retry, alternate-node
  selection, or local fallback.
- The co-located `authority dev-mint` bridge derives current self-attested proof
  bundles for open, execute, and the bounded checkpoint-recovery path. Each can
  optionally mint and submit in one invocation so the four-second development
  capacity evidence is not exposed to a manual delay. It is not discovery,
  independent runtime observation, production enrollment, or automatic recovery
  policy.
- The deterministic source-release allowlist requires
  `src/hosted_remote/v2/dev.rs` together with the V2 protocol, cryptography,
  authorizer, client, server, runtime, and store modules, so the documented
  development bridge is not omitted from the source archive.
- Hosted Placement V6 is not World membership, Governor admission/commit,
  WorldFS, G1 or G10 evidence, a physical-machine or hardware-isolation proof,
  a global exactly-once protocol, arbitrary project/HGraph-island placement,
  cross-session global-effect isolation, persistent-actor migration, safepoint
  migration, cancellation, or rematerialization. It contains no network
  registry/discovery service, automatic scheduler, production authority
  enrollment, multi-key placement chain, automatic GC, retry, or fallback. Its
  exact boundary is documented in
  [`HOSTED_PLACEMENT_V6.md`](HOSTED_PLACEMENT_V6.md).
- Native value crossings are conservative: `Fidelity::NativeCapsule` in
  `src/value.rs` and `src/hgraph/solve.rs` prevents claiming general
  cross-runtime native value soundness.
- Hosted crossing fidelity is derived from typed `BackendValueCapabilities`
  embedded in the canonical backend specification, not from a language-name
  allowlist. Aliases resolve through the same specification. Unknown integer or
  rich-number capability yields `Unsupported`; an abstract `I64` crossing to a
  53-bit float-only backend records structural numeric loss rather than an
  optimistic `Lossless` result.
- Integer exactness is an explicit interval guarantee. `ExactMagnitudeBits(b)`
  denotes `[-2^b, 2^b]`, `TwosComplementBits(b)` denotes
  `[-2^b, 2^b - 1]`, and `ExactRange { min, max }` stores an inclusive
  arbitrary-precision interval. JavaScript/Matlab-style consecutive floating
  integers retain the symmetric 53-bit form; signed fixed-width catalog
  entries use `TwosComplementBits(63)`. Boundary tests cover both signs at
  2^63 and preserve the JavaScript 2^53 behavior.
- The stratified solver stores `FidelityAssessmentV2`, separating losses that
  occur for every represented value (`definite`) from losses possible for at
  least one represented value (`possible`). Sequential crossings union both
  bounds; mutually exclusive abstract paths intersect definite losses and
  union possible losses. The compatibility `Fidelity` projection reports all
  possible losses, so a V1 consumer cannot receive an optimistic answer.
- Structural fidelity loss is non-empty by construction. A legacy serialized
  empty structural set normalizes to `Lossless`, so semantically identical
  states cannot retain two evidence encodings. Composition is covered by
  identity, idempotence, commutativity, and associativity checks.
- `BackendMorphismV1` is an additive shadow kernel, not an admission claim. Its
  Python profile is bounded to recursive acyclic plain data; it rejects
  cycles/shared references, non-string or duplicate map keys, excessive depth,
  and unsupported native objects. Its JavaScript profile has executable
  evidence for recursive profiled-JSON stdout and scalar bindings, not native
  recursive container bindings. Its Rust profile has executable evidence for
  bounded scalar source constants plus profiled stdout, not arbitrary Rust
  program, type, ABI, or binding equivalence. `inject` names conversion of an
  already acquired profiled backend output into O; it is not an assertion that
  the backend can accept that value as input. Both legs carry explicit
  compositional fidelity and their exact boundary descriptions appear in each
  shadow assessment.
- These shadow profiles are not fields of backend-catalog V4 and therefore do
  not change its specification digests, placement compatibility, evidence, or
  protocol bytes. Enforcing them later requires an explicit catalog/evidence
  version rollover. `tests/backend_morphism_v1.rs` exercises a real nested
  Python input binding, a real JavaScript scalar input binding, and the exact
  emitted Rust scalar program through Python, Node.js, and rustc; kernel unit
  tests cover negative values and numeric bounds.
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
  consensus, WorldFS, typed OSTADIX Alpha attestation, Workstream A acceptance,
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

## OSTADIX Alpha native boundary

- [`OSTADIX_WORLD.md`](OSTADIX_WORLD.md) is the normative native constitution.
  It fixes the replicated-Governor model, OValue/capability/capsule crossings,
  explicit aggregate-memory model, fifteen workstreams, and G0--G13
  convergence ladder. Defining that target is not evidence that a gate passed.
- A byte-sealed historical comment in that constitution still names 24
  component gates. The current unsealed `evidence/gates.toml` component
  manifest and generated README projection define 26 required portable QEMU
  gates. The old count is preserved as sealed history, not treated as current.
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
  machine handle or OSTADIX HVC to the guest and carries no handle MAC/key
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
- Neither the present repository nor the OSTADIX Alpha target claims coherent
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
- `olangc --target ir --execution-intent-json` emits the authority-free
  `oexec.execution-intent/v1` projection. It binds exact source bytes, lowered
  OIR, canonical plan, solved analyzed graph, the plan-specific backend-catalog
  projection, analyzer identity, and base policy. Supplying its source and
  intent digests with `O --require-source-sha256` and
  `--require-execution-intent-sha256` makes graph execution recompute and
  compare that projection before dispatch; a match still proceeds through a
  fresh process-local V5 `AdmittedExecution`. The stable intent deliberately
  excludes runtime discovery, backend artifacts, environment and PID state,
  capacity, authority, and live admission, so it is sameness evidence rather
  than a capability or reusable admission token. The gate adds no work to an
  ordinary `O` invocation when its two flags are absent.
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
