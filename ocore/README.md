# O-core

O-core is O-lang's statically typed, freestanding systems language. It has a
separate compiler pipeline from orchestration OIR:

```text
.oc -> AST -> typed HIR -> SSA MIR -> x86_64 ELF object
```

The normative language, layout, ABI, unsafe, atomic, assembly, linkage, and
capability contracts are in [`docs/OCORE.md`](../docs/OCORE.md).

## Compiler

```bash
cargo build --bin ocorec

# Inspect typed layout and name resolution
target/debug/ocorec module.oc --emit hir -o -

# Inspect SSA MIR
target/debug/ocorec module.oc --emit mir -o -

# Emit freestanding x86_64 ELF object and retain assembly
target/debug/ocorec module.oc --emit obj --keep-asm -o module.o
```

Multiple input files form one compilation unit. Each starts with a unique
`module name;` declaration and may import items with `use path::item;`.

## Bootable kernel proof

The kernel example contains no Python, JSON, subprocess, filesystem, libc, or
Rust runtime dependency. Those tools are used only by the hosted compiler and
test harness.

```bash
./ocore/kernel/build.sh       # build target/ocore-kernel/kernel.elf
./ocore/kernel/run-qemu.sh    # interactive serial console
./ocore/kernel/smoke-qemu.sh  # four-second asserted smoke test
```

The asserted output is:

```text
O-core kernel: serial online
page allocator: online
capability: online
T
```

`T` is printed by the IRQ0 timer handler after the IDT, 8259 PIC, and PIT have
been initialized. The handler performs an atomic tick increment and returns
with `iretq`.

The capability runtime uses generation-tagged table slots. Syscall handles do
not expose kernel pointers, and `kernel_syscall_dispatch` validates generation
and rights before dispatching `debug_write`. On the hosted side,
`ocore::capability_bridge::CapabilityBroker` maps live `OCapability` bearer
tokens to those handles and rejects forged, stale, wrong-kind, or insufficient-
rights values before invoking a session transport.

## Current boundary

This is the first vertical slice, not yet a self-hosting general-purpose
compiler. It is x86_64-only, uses a stack-spill backend, and currently requires
aggregate arguments/returns to travel through pointers. Indirect function
calls, enum pattern matching, floating-point computation, ring-3 entry, and a
reclaiming page allocator remain follow-on work. The implemented subset is
enough to compile a freestanding ELF kernel, enter long mode, service IRQ0,
allocate page frames, and enforce generation-tagged capability rights.
