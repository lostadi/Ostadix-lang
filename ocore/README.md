# O-core

O-core is O-lang's statically typed, freestanding systems language. It has a
separate compiler pipeline from orchestration OIR:

```text
.oc -> AST -> typed HIR -> SSA MIR -> target ELF object
```

The normative language, layout, ABI, unsafe, atomic, assembly, linkage, and
capability contracts are in [`docs/OCORE.md`](../docs/OCORE.md).
The staged O-Domain and foreign-personality roadmap, including strict claim
boundaries for the current `native[0]` proof, is in
[`docs/ODOMAIN_PLAN.md`](../docs/ODOMAIN_PLAN.md).
The planned package manager, serial O control plane, activation transaction,
and compiler bootstrap are specified in
[`docs/LIVE_SYSTEM.md`](../docs/LIVE_SYSTEM.md). The security-critical bounded
memory interface, including the first implemented bounded-copy native slice and
the remaining personality-RPC/pinning gates, is specified separately in
[`docs/PERSONALITY_MEMORY_VIEW.md`](../docs/PERSONALITY_MEMORY_VIEW.md).
The host-verified KernelWorld package contract, native normal-form admission,
and nonexecuting VM object boundary are documented in
[`docs/KERNEL_WORLD_CONTRACT.md`](../docs/KERNEL_WORLD_CONTRACT.md).

## Compiler

```bash
cargo build --bin ocorec

# Inspect typed layout and name resolution
target/debug/ocorec ocore/examples/minimal.oc --emit hir -o -

# Inspect SSA MIR
target/debug/ocorec ocore/examples/minimal.oc --emit mir -o -

# Emit freestanding x86_64 ELF object and retain assembly
target/debug/ocorec ocore/examples/minimal.oc --emit obj --keep-asm -o target/minimal.o

# Emit the bounded freestanding AArch64 scalar subset
target/debug/ocorec ocore/examples/minimal.oc \
  --target aarch64-unknown-none --emit obj --keep-asm -o target/minimal-aarch64.o
```

Multiple input files form one compilation unit. Each starts with a unique
`module name;` declaration and may import items with `use path::item;`.

## Bootable kernel proof

The kernel example contains no Python, JSON, subprocess, filesystem, libc, or
Rust runtime dependency. Those tools are used only by the hosted compiler and
test harness.

The build script accepts `rust-lld`, `ld.lld`, or Homebrew `lld`. Set
`OCORE_LLD=/absolute/path/to/rust-lld-or-ld.lld` if your linker is installed
outside the normal Rust, `PATH`, or Homebrew locations.

```bash
./ocore/kernel/build.sh       # build target/ocore-kernel/kernel.elf
./ocore/kernel/run-qemu.sh    # interactive serial console
./ocore/kernel/smoke-qemu.sh  # four-second asserted smoke test
./ocore/kernel/smoke-faults-qemu.sh # fault and user-copy recovery matrix
./ocore/kernel/smoke-processes-qemu.sh # M1 isolation and teardown matrix
./ocore/kernel/smoke-scheduler-qemu.sh # M2 thread/scheduler lifecycle
./ocore/kernel/smoke-ipc-foundation-qemu.sh # M3 mechanism regression
./ocore/kernel/smoke-ipc-qemu.sh # M3 public CPL3 IPC and containment
./ocore/kernel/smoke-loader-qemu.sh # M4 OVFS and static ELF lifecycle
./ocore/kernel/smoke-live-qemu.sh # M5 mode-16 activation and one pkgd restart
./ocore/kernel/smoke-live-semantics-qemu.sh # M5 mode-17 state-machine corpus
./ocore/kernel/build-m6-artifacts.sh # deterministic four-ELF M6A OVFS image
./ocore/kernel/smoke-personality-qemu.sh # M6A mode-18 scalar supervision
./ocore/kernel/smoke-m6b-qemu.sh # M6B mode-19 bounded-copy/revocation mechanism
./ocore/kernel/smoke-live-bounded-personality-qemu.sh # M6B mode-24 live four-byte bounded personality RPC
./ocore/kernel/smoke-live-linux-personality-qemu.sh # mode-25 exact static Linux ELF/CPL3 bounded personality
./ocore/kernel/smoke-live-linux-plan9-qemu.sh # mode-26 Linux ELF data through a Plan-9-style 9P2000 CPL3 client/server path
./ocore/kernel/smoke-world-identity-qemu.sh # mode-27 exact Rust/.oc OWIDENT v1 identity corpus
./ocore/kernel/smoke-world-protocol-qemu.sh # mode-28 exact Rust/.oc OWPROTO v1 codec corpus
./ocore/kernel/smoke-world-value-qemu.sh # mode-29 exact Rust/.oc OWVALUE v1 byte/hash corpus
./ocore/kernel/smoke-world-receipt-qemu.sh # mode-30 exact Rust/.oc OWRECEIPT v1 receipt/preimage corpus
./ocore/kernel/smoke-world-project-runtime-qemu.sh # mode-32 hosted receipt generation plus native semantic comparison
./ocore/kernel/smoke-kernel-world-qemu.sh # mode-20 native admission/nonexecuting VM objects
./ocore/kernel/smoke-kernel-world-execution-qemu.sh # mode-21 AMD SVM/NPT execution; requires nested SVM + /dev/kvm
./ocore/kernel/smoke-aarch64-g2-qemu.sh # World G2 native AArch64 EL0/IPC/capability/lifecycle gate under forced TCG
```

The asserted default `smoke-qemu.sh` output is:

```text
O-core kernel: serial online
page protections: W^X online
page allocator: online
M03 frames: reclaim PASS
M03 frames: zero-reuse PASS
M03 frames: stale-double-free denied
M03 frames: injected-failure rollback PASS
M03 memory objects: typed-generation PASS
address space: online
capability: online
user copy faults: recovered
entry state: CPU-local online
T
CPL3 native[0]: online
user zero-fill: online
capability bounds: denied
forged capability: denied
stale capability: denied
wrong rights: denied
wrong type: denied
closed capability: denied
user ranges: denied
kernel pointer: denied
unknown syscall: denied
register preservation: online
cap_copy reserved: denied
process exit gated: denied
M03 page_alloc: capability online
M03 quota: enforced-recovered
M03 memory stale close: denied
M03 memory lifecycle: PASS
oversized buffer: denied
RFLAGS sanitization: online
timer CPL3 return: online
yield hook: online
CPL3 heartbeat: online
QEMU smoke: PASS
```

`T` is printed by the IRQ0 timer handler after the IDT, 8259 PIC, and PIT have
been initialized. The gate requires that standalone line before a CPL3
timer-return marker and a later heartbeat. Because syscall entry masks IF, the
tick count can advance during the probe loop only at CPL3; continuing to both
later markers proves the TSS ring-0 transition and `iretq` return.

The capability runtime uses object-typed, generation-tagged slots selected by
the current process's kernel-owned cspace identity. Syscall handles do not
expose kernel pointers, and `kernel_syscall_dispatch` validates object type,
generation, rights, and the current PCB's concrete readable region before
copying into a kernel buffer for `debug_write`. The CPL3 probe covers bounds, occupancy, wrong
generation, stale-after-reuse, wrong rights, wrong type, close, crossing-end,
wrapping-length, and kernel-pointer denial. On the hosted side,
`ocore::capability_bridge::CapabilityBroker` maps live `OCapability` bearer
tokens to those handles and rejects forged, stale, wrong-kind, or insufficient-
rights values before invoking a session transport. Its public API is
operation-specific, so callers cannot understate rights or choose a different
syscall while asking the broker to authorize it.

The physical allocator now tracks and reclaims the 3,072 frames in the fixed
4..16 MiB supervisor-only QEMU bootstrap window. Frame and memory-object handles
have disjoint internal namespace tags and generations. Final release zeros a
page, and executable, anonymous, shared, kernel, page-table, and rejected device
kinds cannot be confused by integer coincidence. The default smoke test
exhausts and reclaims the complete pool, tests frame and object refcounts,
rejects stale and double release, and verifies rollback after injected failures.

`page_alloc` accepts only anonymous or shared allocation through a page-pool
capability, applies a per-CSpace quota, chooses the destination CSpace slot in
the kernel, and returns that generated capability. Executable allocation is
kernel/loader-only, device memory is rejected from the RAM pool, and no physical
address crosses the ABI. Closing the final memory capability reclaims its frame.

The bootstrap now uses page-granular RX, R/NX, and RW/NX supervisor mappings,
real user and privileged stack guards, CPU-local `SWAPGS` entry storage, 32
normalized exception stubs, and exact page-fault fixups for bounded user copy.
`debug_write` copies through a 256-byte kernel bounce buffer. The separate
fault smoke test covers divide error, invalid opcode, non-present read,
supervisor read, guard-stack write, NX instruction fetch, noncanonical target,
and an excluded syscall-return RIP. Every run requires process 1 to become
`FAULTED`, the current process to clear, and a later kernel timer marker.
The final mode removes one otherwise valid user-image leaf and proves that the
syscall copy returns `ERR_USER_COPY_FAULT`, then resumes through a later CPL3
heartbeat without faulting the process.

That one-process wording describes the Milestone 0.2 fault matrix, not the
current upper bound. Milestone 1 is complete for two bounded native processes
on one CPU. Its gate boots separate normal-exit and contained-fault scenarios
and requires independent CR3s, same-VA physical isolation, an atomic
PCB/domain/address-space/CSpace switch, split teardown, stale identity denial,
sibling survival, frame reclamation, and a post-lifecycle timer.

Milestone 2 is complete for four TCBs across two processes on one CPU. Its gate
requires one million forced identity transactions, FIFO runnable and blocked queues,
progress from two CPU-bound and two sleeping CPL3 threads, cooperative yield,
timer preemption, cross-thread hostile-RFLAGS sanitization, wake-once sleep,
priority/accounting checks, hostile saved-RSP TCB containment, idle entry, exit
during preemption, sibling progress, stale TCB denial, frame reclamation, and a
post-lifecycle timer. The million-iteration stress does not enter CPL3; the
bounded IRQ/SYSCALL phase separately proves real save/restore and IRETQ switches.
Native ABI v1 assigns syscall 6 to lifecycle-gated `exit`
and syscall 7 to scheduler-gated `sleep`; `yield` performs a real scheduling
transition while the scheduler is active.

Milestone 3 has a bounded public CPL3 IPC gate. It proves endpoint
create/send/receive/cancel, real full-FIFO TCB block/wake, preemptive
cross-domain request/reply, generation-safe attenuated transfer, automatic
dead-sender cleanup, and a contained personality-service crash with unrelated
world progress. Transfer tickets bind the exact creating process generation and
the endpoint-derived destination CSpace, not the endpoint object. The gate also
exhausts all 16 ticket records, denies abort by another process, proves
owner-only exact-once abort and stale-ticket denial, and verifies a fresh
prepare after recovery. The original foundation gate remains as a mechanism
regression.

Milestone 4 imports a deterministic immutable OVFS image, recomputes its
SHA-256 in-kernel, rejects malformed/overlapping/W+X static ELF files, and runs
two independently linked personalities at the same virtual addresses in
separate W^X CR3 roots. Service lookup returns a capability; namespace teardown
and complete frame reclamation are part of the gate.

Milestone 5 loads four separately linked native service ELFs into isolated
CSpaces. A real CPL3 serial REPL owns the only typed control capability and
drives immutable-digest install and health-gated activation. The mode-16 gate
then contains a real package-daemon CPL3 fault, withdraws that generation in
`CONTROL_RECOVERING`, and republishes a freshly loaded generation only after its
exact health token, before final deactivation and reclamation. The independent
mode-17 semantics boot covers overgrant denial, failed health, rollback, stale
service generations, restart, and strict command parsing. Package and
supervisor policy is still privileged and fixed-capacity rather than
endpoint-backed user-space daemon policy; the proof is one restart generation,
not a general retry or backoff system.

M6A loads a test client, native personality daemon, native supervisor daemon,
and unrelated observer from one deterministic digest-pinned OVFS image into
four isolated W^X address spaces and CSpaces. The unprivileged supervisor
health-gates publication and chooses cancellation, one crash-driven restart,
and cooperative stop; O-core supplies scalar routing, exact terminal
arbitration, containment, reload, and call-capability rebind as mechanism. The
mode-18 gate proves stale, late, duplicate, and prior-generation denial plus
complete reclamation and later timer survival. It is not full Milestone 6: no
pointer-bearing foreign memory view or foreign operating-system ABI is present.

Mode 19 is the first bounded M6B mechanism slice. It creates generation-tagged,
request-scoped bounded-copy views over a real kernel process/address space, with
kernel-owned staging, direction-attenuated nontransferable capabilities,
snapshot input, and written-prefix-only output commit. The fixed limits are
four views, 128 bytes per view, and 256 charged bytes total. Reply,
cancellation, timeout, service-death, process-exit, unmap, and resource
revocation hooks close the capability before one terminal result and one wake
publication. Post-reply process-exit/unmap cleanup publishes neither again.
Typed revocable leases carry exact request identities across view binding,
cover memory, filesystem, timer, network, and device classes without ambient
fallback, and the gate proves request-wide revocation leaves an unrelated
request alive. The gate also injects a bind failure after lease capability
publication and proves transactional revoke-and-destroy rollback.
Mode 19 itself remains a directly exercised mechanism gate. Mode 24 separately
wires one exact four-byte `INOUT` request through the live CPL3 client,
personality daemon, router, and supervisor. Four digest-pinned ELFs cross the
public bounded-call/view/reply syscalls, contain one generation-1 daemon fault,
and health-gate a generation-2 rebind. The generation-2 supervisor then selects
pre-terminal unmap, request-revoke, delegated-device-resource-revoke, and
caller-exit dispositions while each request is still waiting. These are
supervisor-triggered lifecycle dispositions: the gate does not mutate a
mapping or observe an external resource event, and its device authority is one
internal delegated lease rather than a physical device. It also does not cover
the post-reply/pre-consume process-exit or unmap race, pinned windows,
signal/restart integration, a Linux oracle, or concrete services.

Mode 25 separately packages one exact 8,520-byte static Linux x86-64 ELF with
three native service principals in a 60,104-byte digest-pinned OVFS image. The
foreign ELF enters at CPL3, performs exact fd 1/fd 2 writes through
request-scoped bounded `IN` views, observes Linux `-ENOSYS`, and exits with
status 42. Its generation-1 daemon completes stdout before faulting; the closed
view authority and committed terminal result are preserved across
generation-1 service/fd withdrawal until the client consumes that result after
generation-2 publication. The replacement first proves stale generation-1
lookup denial during private startup, then answers health and is published;
only afterward does the client consume stdout and proceed to stderr. The gate
also proves unrelated-observer survival, full reclamation, and a later timer.
It is not Linux or Plan 9 boot, a
distribution, a root filesystem, a dynamic linker, a general foreign ABI,
KVM/SVM hardware evidence, PCI/device assignment, DMA/IOMMU isolation, or
physical-device evidence.

Mode 26 composes that same exact Linux-ABI ELF with an actual bounded 9P2000
wire path. `build-m7-linux-plan9-artifacts.sh` deterministically packages
`/bin/linux-minimal.elf`, `/sbin/linux-9pd.elf`,
`/sbin/linux-supervisord.elf`, and `/bin/plan9-namespace-client.elf` into a
92,872-byte immutable OVFS image with SHA-256
`920b014cfb133f033b6761da6fe5b1d22be613bf88112c05ec0af982e1beebd9`.
All four principals load as data into isolated W^X CPL3 address spaces and
CSpaces. The Linux ELF produces the exact generation-1 stdout and generation-2
stderr snapshots; the unprivileged native 9P server exposes only the
generation-bound `/srv/linux/status` path. An independently linked native
Plan-9-style client negotiates `9P2000` with `msize = 128` and performs exact
version, attach, walk, open, read, and clunk exchanges. Its first generation
also verifies sequence, version, path, mode, and count errors. The server then
faults deliberately, O-core withdraws the generation-1 namespace and call
authority, both clients survive, and a health-gated replacement serves
generation 2 only after stale generation-1 capability denial. The supervisor
stops the replacement after Linux `-ENOSYS` and `exit_group(42)` complete;
bounded authority and resources are reclaimed before a later timer fires.

Run this live Linux-to-9P evidence gate with:

```bash
./ocore/kernel/smoke-live-linux-plan9-qemu.sh
```

The Plan-9-style client and 9P server are native O-core CPL3 principals, not a
Plan 9 kernel or Plan 9 binary. Mode 26 does not boot Linux or Plan 9, provide a
distribution, root filesystem, dynamic linker, general Linux ABI, general 9P
server, namespace or mount environment, network transport, persistent
filesystem, or guest-agent framework. QEMU TCG execution is not KVM/SVM or
physical-hardware evidence, and the gate has no PCI/device assignment, DMA,
IOMMU, interrupt-remapping, hardware-reset, or physical-device proof.

Mode 20 carries a host-verified `ocore.kernel-world/v1` package contract into a
bounded native supervisor-admission gate. The host emits a deterministic,
hash-pinned `OKWORLD1`
normal form; O-core verifies its exact record digest before strict parsing,
preserves distinct package/manifest digests, and admits requests only through
independently registered exact-package plus byte-exact kind/purpose policy with
default denial. String hashes are only a fast reject and never authority.
The resulting generation-bound VM/vCPU/guest-page objects are nonexecuting.
Their local pilot graph can be sealed while package admission remains
`ADMITTED`; this is not a provider configuration/start transition or proof of
full manifest-resource fulfillment. They prove aligned anonymous page backing,
quota/overlap denial, stale identity,
exact-world reclamation, unrelated-VM survival, and a later timer, but no guest
boot, VMX/SVM, EPT/NPT, firmware, interrupt injection, device assignment, DMA,
or IOMMU behavior.

## Current boundary

This is not yet a self-hosting general-purpose compiler. The broad kernel and
runtime remain x86_64; G2 adds a separate conservative scalar AArch64
stack-spill backend and one bounded native image. Both targets currently require
aggregate arguments/returns to travel through pointers. Indirect function
calls, enum pattern matching, floating-point computation, and executable
loading beyond the current bounded static-ELF gates remain follow-on work.
Float operations, casts, and
`sysv64` float crossings are rejected during type checking, so the layout-only
float types cannot reach integer machine operations.

The current verified kernel boundary includes G2's resident AArch64 EL2 vector,
one domain-separated EL1 HVC/ERET integrity round trip, and its bounded MMU-off
EL0 corpus under QEMU TCG. It does not install stage-2 translation or boot a
foreign kernel. The boundary also includes M6A's scalar CPL3 supervision in
mode 18, M6B's bounded-copy/revocation mechanism in mode 19, Mode 24's exact
four-byte live bounded personality composition, Mode 25's exact static Linux
ELF/minimal-ABI composition, Mode 26's bounded Linux-to-9P2000 CPL3 service
path, Mode 27's shared `OWIDENT` identity corpus, Mode 28's bounded `OWPROTO`
record-codec corpus, Mode 29's separate bounded `OWVALUE` portable-value and
full-record SHA-256 corpus--19 records and 928 bytes, with concatenated SHA-256
`264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc`--and the nonexecuting KernelWorld admission/object
slice in mode 20. Mode 30 adds a separate bounded `OWRECEIPT` canonical
receipt/signing-preimage oracle with an algorithm-tagged signature envelope. Its
fixed two-record corpus is 3,239 bytes (6,478 lowercase hex digits) with SHA-256
`1edd90bf881cd42d08e2031482baae4e7c9a95bd78cfa65f0cbe14147c0a2604`; the
current and stale signing preimages are 1,575 and 1,546 bytes respectively.
Hosted Rust verifies Ed25519 using a pinned public conformance key; native O-core
validates the receipt and envelope structure but is not a general freestanding
Ed25519 verifier. Mode 32 consumes one live hosted-reference project receipt,
fully decodes and canonically reencodes it, constructs its validated signing
preimage, requires an `Uncommitted` fence, and compares the domain-separated
SHA-256 of its canonical unsigned body with hosted Rust. It then reuses the
successful validation scratch with a malformed envelope and proves the prior
terminal/commit tags were cleared. The required no-argument gate is
`smoke-world-project-runtime-qemu.sh`; it generates a hosted vector and calls
the direct two-argument `smoke-world-project-receipt-qemu.sh` vector interface
with `RECEIPT_HEX_FILE EXPECTED_SEMANTIC_SHA256`. Modes 27 through 30 and 32
are byte-level schema/semantic oracles, not a transport,
authenticated authority path, Governor, consensus system, or World Alpha
qualification.
Mode 29 uses a 4096-byte maximum, depth-16 and 128-node limits,
canonical records and scalar-key maps, and root-only inert extensions. It
rejects hosted authority, capsules, live references, requests, and other
effectful forms; it does not make the full hosted `OValue` portable, change the
hosted canonical-CBOR shim, or provide a live crossing. Mode 30 remains a fixed
offline corpus; the separate hosted project adapter now emits a caller-signed
OWRECEIPT bound to a terminal RuntimeGraph, but always with an explicit
`Uncommitted` fence. That receipt places a caller-supplied coordinator observer,
uses a dedicated coordinator attempt distinct from every operation attempt, and
leaves its package subject absent; the proposed provider and implementation
remain descriptive launch/RuntimeGraph fields rather than receipt placement or
a fabricated package. The RuntimeGraph is causally replayed, uses neutral
`RouteSettlement`, and aggregates residual `HostWorld` over observed execution.
Mode 32 does not execute that project in O-core or verify Ed25519 natively, and
neither signature nor semantic equality grants authority or establishes
Governor admission/current World state. The kernel remains
single-CPU, fixed-window, static-ELF, and host-built: there is no firmware RAM
discovery, demand paging, general user mapping, SMP locking, FPU/SIMD context,
dynamic linker, writable general filesystem, general foreign ABI personality,
foreign root filesystem, native compiler/self-hosting, general guest-agent
transport, or live hosted-broker transport into QEMU. Modes 24 through 26 do
not boot Linux or Plan 9. Mode 25 covers only its pinned four-call success path
and fifth failure-only exit site; Mode 26 adds one exact native 9P2000 client
and server corpus rather than a Plan 9 binary or general namespace.
The early bootstrap/fault gates still use a linked `native[0]` payload; later
claims have separate bounded gates.

The x86_64 backend rechecks MIR operand, result, call, branch, index, atomic,
volatile, and assembly contracts so unsupported type shapes fail instead of
falling through to integer-shaped instructions.
The AArch64 backend supports AAPCS64-like compiler-versioned `extern "ocore"`
scalar calls and `syscall0` through `syscall6`, and fails closed on AMD64
`sysv64`, atomics, inline assembly, interrupt/naked functions, floating point,
and calls with more than eight integer arguments.
