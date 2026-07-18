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
```

The asserted output is:

```text
O-core kernel: serial online
page protections: W^X online
page allocator: online
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
reserved syscalls: denied
oversized buffer: denied
RFLAGS sanitization: online
timer CPL3 return: online
yield hook: online
CPL3 heartbeat: online
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

## Current boundary

This is the first vertical slice, not yet a self-hosting general-purpose
compiler. It is x86_64-only, uses a stack-spill backend, and currently requires
aggregate arguments/returns to travel through pointers. Indirect function
calls, enum pattern matching, floating-point computation, independent
per-process page tables, executable loading, preemptive scheduling, and a
reclaiming page allocator remain follow-on work. Float operations, casts, and
`sysv64` float crossings are rejected during type checking, so the layout-only
float types cannot reach integer machine operations. The implemented subset is
enough to compile a freestanding ELF kernel, enter long mode, run one statically
linked `native[0]` task at CPL3, cross a real `SYSCALL` boundary, service IRQ0,
contain expected faults from that task, allocate page frames, and enforce
process-bound capability and mapping-aware user-pointer checks. The ELF
reserves and zero-fills the complete bootstrap user image and mapped stack
range. Fault containment proves controlled disposition of the only process,
not survival of an isolated sibling. It does not yet run Linux binaries or any
other foreign OS personality.
The x86_64 backend rechecks MIR operand, result, call, branch, index, atomic,
volatile, and assembly contracts so unsupported type shapes fail instead of
falling through to integer-shaped instructions.
