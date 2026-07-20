# O-core

O-core is O-lang's statically typed, freestanding systems language. It has a
separate compiler pipeline from orchestration OIR:

```text
.oc -> AST -> typed HIR -> SSA MIR -> x86_64 ELF object
```

The normative language, layout, ABI, unsafe, atomic, assembly, linkage, and
capability contracts are in [`docs/OCORE.md`](../docs/OCORE.md).
The staged O-Domain and foreign-personality roadmap, including strict claim
boundaries for the current `native[0]` proof, is in
[`docs/ODOMAIN_PLAN.md`](../docs/ODOMAIN_PLAN.md).
The planned package manager, serial O control plane, activation transaction,
and compiler bootstrap are specified in
[`docs/LIVE_SYSTEM.md`](../docs/LIVE_SYSTEM.md). The security-critical bounded
memory interface for future personality services is specified separately in
[`docs/PERSONALITY_MEMORY_VIEW.md`](../docs/PERSONALITY_MEMORY_VIEW.md).

## Compiler

```bash
cargo build --bin ocorec

# Inspect typed layout and name resolution
target/debug/ocorec ocore/examples/minimal.oc --emit hir -o -

# Inspect SSA MIR
target/debug/ocorec ocore/examples/minimal.oc --emit mir -o -

# Emit freestanding x86_64 ELF object and retain assembly
target/debug/ocorec ocore/examples/minimal.oc --emit obj --keep-asm -o target/minimal.o
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

## Current boundary

This is the first vertical slice, not yet a self-hosting general-purpose
compiler. It is x86_64-only, uses a stack-spill backend, and currently requires
aggregate arguments/returns to travel through pointers. Indirect function
calls, enum pattern matching, floating-point computation, and executable
loading beyond the current bounded static-ELF gates remain follow-on work.
Float operations, casts, and
`sysv64` float crossings are rejected during type checking, so the layout-only
float types cannot reach integer machine operations.

The current verified kernel ceiling is the bounded scalar M6A slice in mode 18
described above. It remains single-CPU, fixed-window, static-ELF, and host-built:
there is no firmware RAM discovery, demand paging, general user mapping, SMP locking,
FPU/SIMD context, dynamic linker, writable general filesystem, foreign ABI
personality, foreign root filesystem, native compiler/self-hosting, or live
hosted-broker transport into QEMU. The early bootstrap/fault gates still use a
linked `native[0]` payload; M1 through M5 and M6A have separate lifecycle gates.

The x86_64 backend rechecks MIR operand, result, call, branch, index, atomic,
volatile, and assembly contracts so unsupported type shapes fail instead of
falling through to integer-shaped instructions.
