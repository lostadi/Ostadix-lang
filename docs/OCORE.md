# O-core Language and Freestanding Runtime Specification

Status: draft v0.1, normative for the `ocorec` implementation.

O-core is the native systems-programming member of O-lang. It is deliberately
separate from the polyglot orchestration language and its OIR. O-core programs
are statically typed and compile ahead of time to target object files. Foreign
language blocks (`python^`, `rust^`, and similar) are hosted facilities and are
never available in freestanding O-core code.

## 1. Compilation model

The native pipeline is:

```text
O-core source -> AST -> resolved, typed HIR -> SSA MIR -> target object
```

The existing orchestration pipeline remains:

```text
.O source -> ONode -> OIR execution plan -> hosted evaluator/backends
```

OIR and O-core MIR are different representations with different invariants.
OIR models dependency and backend execution. MIR models typed machine-level
computation. Neither is implicitly converted to the other.

The initial target is `x86_64-unknown-none`. Its object format is ELF64, its
data model is LP64, and its default calling convention is System V AMD64.

## 2. Source units and modules

Every source file begins with a module declaration:

```ocore
module kernel::serial;
use kernel::arch::outb;
```

An invocation of `ocorec` is one compilation unit and may contain multiple
files. Module names must be unique. Unqualified names resolve in this order:
local bindings, items in the current module, explicitly imported items, and
predeclared intrinsics. Cross-module symbols are mangled unless marked
`@export` or `@no_mangle`.

## 3. Items and control flow

The item forms are functions, extern functions, structures, enumerations,
constants, and statics:

```ocore
struct Slice { data: *const u8, len: usize }
enum Poll { pending, ready(u64), failed(i32) }

const PAGE_SIZE: usize = 4096;
static mut NEXT_PAGE: usize = 0x0020_0000;

extern "sysv64" fn boot_info() -> *const u8;

@export
@link_section(".text.kernel")
unsafe fn kernel_main() -> never {
    loop { asm!("hlt", options(nomem, nostack)); }
}
```

Statements include `let`, assignment, expression statements, `if`/`else`,
`while`, `loop`, `break`, `continue`, `return`, and `unsafe` blocks. Functions
have lexical scope and no implicit fallthrough return unless their return type
is `void`.

## 4. Types

Primitive types are:

```text
bool
u8 u16 u32 u64 usize
i8 i16 i32 i64 isize
f32 f64
void never
```

Compound types are arrays `[T; N]`, immutable and mutable raw pointers
`*const T` and `*mut T`, structures, enumerations, and function pointers
`fn(T, U) -> R`.

There are no implicit numeric conversions except that an unsuffixed integer
literal may be inferred from its expected integer type. All other conversions
use `as`. Pointer-to-integer, integer-to-pointer, pointer arithmetic, pointer
dereference, mutable static access, inline assembly, and privileged intrinsics
require an unsafe context.

Floating-point types currently provide storage layout and bit-preserving
transport only. Float literals, arithmetic, comparisons, casts, and `sysv64`
float parameters and returns are compile-time errors until SSE lowering and the
floating-point calling convention are implemented. This boundary is enforced
in type checking and defended again in machine code generation.

O-core v0.1 has no garbage collector and no implicit heap allocation. Values
have deterministic destruction-free storage. A later ownership layer may add
checked owning pointers, but it must lower to this explicit storage model.

## 5. Layout and ABI

Primitive sizes and alignments for `x86_64-unknown-none` are:

| Type | Size | Alignment |
|---|---:|---:|
| `bool`, `u8`, `i8` | 1 | 1 |
| `u16`, `i16` | 2 | 2 |
| `u32`, `i32`, `f32` | 4 | 4 |
| `u64`, `i64`, `usize`, `isize`, `f64`, pointer | 8 | 8 |
| `void`, `never` | 0 | 1 |

Structures use declaration order. Each field begins at the next address
aligned for that field; structure size is rounded up to maximum field
alignment. `@packed` removes inter-field padding and gives alignment 1.
`@align(N)` may increase alignment to a power of two.

Enums are a tagged union. The tag is the smallest of `u8`, `u16`, or `u32`
that can represent every variant. The payload begins at its required alignment
after the tag; total size is rounded to maximum tag/payload alignment.

`extern "sysv64"` uses System V AMD64. Integer and pointer arguments use RDI,
RSI, RDX, RCX, R8, R9, with further arguments on the stack. Scalar results use
RAX. Aggregate ABI passing is currently forbidden across both direct and
extern call boundaries; callers pass pointers instead. The stack is 16-byte
aligned before `call`.

`extern "ocore"` is versioned with the compiler and is not a stable foreign
ABI. Interrupt entries use `@interrupt`; they have no ordinary arguments or
return value and end with `iretq`.

## 6. Unsafe and hardware operations

Unsafe operations are syntactically visible:

```ocore
unsafe {
    volatile_store(mmio, value);
    let status: u32 = volatile_load(status_reg);
    outb(0x3f8, byte);
}
```

Freestanding intrinsics are:

- `volatile_load(ptr)` and `volatile_store(ptr, value)`;
- `atomic_load(ptr, order)`, `atomic_store(ptr, value, order)`,
  `atomic_exchange`, `atomic_compare_exchange`, and `atomic_fetch_add`;
- `inb`, `inw`, `inl`, `outb`, `outw`, `outl`;
- `enable_interrupts`, `disable_interrupts`, `halt`, and `invalidate_page`;
- `syscall0` through `syscall6` for user-mode stubs;
- `asm!(template, operands..., options(...))`.

Memory order values are `relaxed`, `acquire`, `release`, `acq_rel`, and
`seq_cst`. Invalid load/release and store/acquire combinations are compile-time
errors. Volatile operations prevent compiler elision and reordering relative
to other volatile operations; they do not provide inter-core synchronization.
The current volatile lowering accepts scalar pointees. Atomic pointees must be
1, 2, 4, or 8-byte integers, and the backend rechecks the pointer, value,
result, and ordering types before emitting x86_64 instructions.

Inline assembly templates use Intel syntax. Input/output registers are
explicit, implicit clobbers are forbidden, and `options(nostack)` asserts that
RSP is unchanged. Assembly operands must be non-floating scalar values because
the current interface exposes general-purpose registers. Assembly is unsafe
even when it contains no privileged instruction.

## 7. Linkage attributes

Supported attributes are:

- `@export`: externally visible symbol;
- `@no_mangle`: use the source identifier as the linker symbol;
- `@link_section("name")`: place a function or static in a named section;
- `@align(N)`: increase item/type alignment;
- `@used`: retain an otherwise unreferenced static;
- `@packed`: packed structure layout;
- `@interrupt`: x86_64 interrupt entry ABI;
- `@naked`: no compiler prologue/epilogue; body is restricted to assembly.

Section names are emitted verbatim. Applying executable section attributes to
writable statics is rejected unless `@unsafe_linkage` is also present.

## 8. Freestanding runtime boundary

The freestanding runtime may depend only on the target ABI and symbols
provided by the kernel image. It may not depend on subprocesses, JSON,
filesystem access, Python, Nix, libc, Rust `std`, environment variables, or a
host allocator.

The runtime supplies boot entry glue, zeroing `.bss`, serial I/O, IDT
installation, a timer interrupt, fixed-window reclaiming frame allocation,
syscall entry, and panic-to-serial. Allocation in interrupt context is forbidden
until a separate interrupt-safe allocator exists. The current allocator is
single-CPU and relies on boot and syscall entry running with interrupts masked.

## 9. Capabilities and syscalls

A generic or deserialized `OCapability` is not kernel authority. An
`OCapability` emitted by a live hosted broker is a bearer for a private session
binding. Kernel authority itself is represented by an unforgeable
`(slot, generation)` handle tied to a per-process capability table:

```text
CapabilityEntry = { object_id, object_type, rights, generation, state }
CapabilityHandle = (generation << 32) | slot
```

Every capability syscall validates slot bounds, live state, generation, object
type, and requested rights. Handles never contain kernel pointers. Kernel-only
transfer transactions can reserve a destination slot without exposing it as a
live capability, then publish or abort that exact reservation. Close similarly
uses a begin/commit/abort state. Committing a close or aborting an unpublished
reservation advances its generation before reuse; aborting an in-progress
close restores the same exact live capability. Exhausted 32-bit generations
retire instead of wrapping to a value that could revive an old handle.

Initial syscall numbers are:

| Number | Operation |
|---:|---|
| 0 | `debug_write(cap, ptr, len)` |
| 1 | `cap_close(cap)` |
| 2 | reserved `cap_copy` number; returns `ERR_NOT_IMPLEMENTED` |
| 3 | `page_alloc(page_pool_cap, kind)`; returns a generated memory capability |
| 4 | `yield()` |
| 5 | `ticks()` |
| 6 | `exit(status)`; enabled only by a trusted lifecycle harness |
| 7 | `sleep(delta_ticks)`; enabled only while the scheduler is active |

`page_alloc` accepts anonymous or shared memory from a typed page-pool
capability. It enforces the current CSpace's hard frame quota and distinguishes
quota exhaustion, physical-frame exhaustion, resource-table exhaustion, and an
invalid memory kind. Executable memory remains kernel/loader-only; device memory
is never allocated from the RAM pool. The returned value is a CSpace capability,
not an internal object handle, frame index, or physical address.

`yield` records a request in every mode. It requests an actual scheduler
transition when the Milestone 2 scheduler is active and returns directly in the
bootstrap gate. `exit` abandons a user frame only after the kernel has installed
a trusted lifecycle continuation. `sleep` rejects zero or out-of-range delays
and returns `ERR_NOT_IMPLEMENTED` when the scheduler is inactive. `cap_copy`
retains its stable number but still returns `ERR_NOT_IMPLEMENTED`; the
Milestone 3 transfer primitive is kernel-only and limited to shared-memory
capabilities.

The hosted `OCapability` wire value may refer to a live kernel capability only
through an authenticated transport endpoint. Its string `identity` is never
accepted directly as a kernel handle.

The hosted `CapabilityBroker` implements this boundary. It generates 256-bit
bearer identities from operating-system entropy and keeps a private per-session
token-to-handle table. Callers use operation-specific `debug_write` and
`cap_close` methods; they cannot select a syscall number, expected kind, or
required-rights mask. A successful kernel close removes the bearer, while a
transport failure or kernel rejection preserves it. Deserialized identities
not already bound in that live broker session are rejected as forged or stale.
Metadata cannot select a slot or add rights.

This prevents guessing, serialized forgery, metadata-based escalation, stale
or revoked token use, and cross-session replay. It does not prevent theft of a
still-live bearer inside the same broker session, broker-process compromise,
or authenticated-transport compromise. Possession of a live bearer is an
explicit delegation of its bounded authority.

## 10. Hosted foreign-language boundary

Foreign blocks remain part of `.O` orchestration. They may construct O-core
source, invoke `ocorec`, link images, launch QEMU, and inspect results. They are
not legal inside `.oc` source and are not linked into freestanding artifacts.
This preserves O-lang's polyglot model without making Python, Rust, Nix, JSON,
or subprocess execution part of the kernel trusted computing base.

## 11. Implemented bounded O-core milestone boundary

The initial compiler targets only `x86_64-unknown-none` and uses a simple
stack-spill backend without optimization or register allocation. Direct calls
are supported; function-pointer types are representable, but indirect calls
are not yet lowered. Aggregates support layout, construction, fields,
indexing, locals, statics, and copies, while aggregate parameters and returns
must currently be passed through pointers. Enum construction is supported;
pattern matching is not. Floating-point types reserve their storage layouts,
while operations, conversions, and `sysv64` ABI crossings are rejected before
MIR lowering. Code generation also validates the MIR type contracts for
operations, calls, control flow, indexed places, atomics, volatile access, and
assembly. This is a second boundary against malformed or future lowering paths
silently selecting integer instructions.

Milestones 0.1 through 0.3 are complete for the bounded single-CPU bootstrap
gate. The kernel enters a linked `native[0]` payload at CPL3, crosses an
architectural `SYSCALL` boundary through CPU-local entry state, validates the
IRET target and RFLAGS, and survives IRQ0 return. Page-granular supervisor
mappings enforce RX, R/NX, and RW/NX roles; guarded user and kernel stacks, a
double-fault IST, normalized exception frames, and exact user-copy fixups bound
the fault surface. The fixed 4..16 MiB QEMU window contains 3,072 reclaiming,
typed, reference-counted frames with generation-safe reuse and zeroing.
Transactional memory objects and per-CSpace quotas back anonymous/shared
`page_alloc` capabilities without exposing object handles or physical
addresses.

`smoke-qemu.sh` is the positive bootstrap and memory-lifecycle gate.
`smoke-faults-qemu.sh` separately boots each fatal Milestone 0.2 probe, requires
the expected trap and later timer, and checks a recoverable missing-PTE user
copy. Its one-process disposition wording applies to that historical bootstrap
scenario only.

Milestone 1 is complete for two bounded native processes on one CPU. Dynamic
processes have generation-tagged domain, process, address-space, mapping, and
CSpace owner identities. Their roots share RX user text and supervisor-only
kernel mappings while using private RW/NX data and guarded stacks. Context
installation changes CR3, TSS.RSP0, GS entry state, PCB, domain, address space,
and CSpace as one transaction. Reap is split across ownership release,
address-space destruction, type-aware CSpace drain, and final generation
advance. `smoke-processes-qemu.sh` boots independent normal-exit and
contained-fault scenarios and requires same-VA physical isolation, stale identity denial,
sibling survival, complete dynamic-frame reclamation, and a post-lifecycle
timer.

Milestone 2 is complete for four TCBs across two processes and one CPU. The
scheduler uses canonical 22-word saved frames, FIFO runnable and blocked queues,
timer preemption, cooperative yield, sleep deadlines, wake reasons, bounded
priority quanta, accounting, and a ring-0 idle path. Its prepare/install/commit
switch transaction keeps registers and guarded stacks aligned with CR3,
TSS.RSP0, GS state, PCB, domain, address space, and CSpace.
`smoke-scheduler-qemu.sh` requires one million forced identity transactions,
progress from two CPU-bound and two sleeping CPL3 threads, wake-once behavior,
cross-thread hostile-RFLAGS sanitization on a syscall-selected TCB, exit during
preemption, hostile saved-RSP TCB containment, sibling progress, stale TCB
denial, frame reclamation, and a post-lifecycle timer. The million-iteration
stress verifies identity installation and a saved-frame canary without entering
CPL3; real frame save/restore and IRETQ switching run in the bounded IRQ/SYSCALL
phase. Failed context installation rolls architectural and PCB identity back to
the verified management state before the prepared TCB is returned to the
runnable queue.

Milestone 3 has a verified foundation and is not complete. The kernel-side
foundation contains eight generation-tagged endpoint objects, four-message FIFO
queues, correlation cancellation, deterministic waiter-record and capability
cleanup, invisible generation-tagged destination-slot reservations,
exact-generation queued-capability escrow, and generation-tagged transfer
tickets. Transfer is atomic, attenuation-only, and currently restricted to
shared-memory objects. Each dynamic address space can optionally map one shared
object at fixed virtual address `0x01202000` as RW/NX, but only through a
`RIGHT_MEMORY_USE` capability in that address space's exact owner CSpace.
`smoke-ipc-foundation-qemu.sh` proves cross-CR3 visibility, bounded queue
backpressure and FIFO order, cancellation and waiter-record cleanup, endpoint
and destination-capability generation reuse, stale ticket denial, exact
attenuation and failed re-transfer, rejection of new work from a dead sender,
explicit management-harness cancellation of its earlier queue item and ticket,
full resource reclamation, and later timer survival.

The waiter records prove bounded registry bookkeeping, not live blocked TCB
transitions. The Milestone 3 foundation exposes no CPL3 endpoint operations.
Empty receive and full send do not yet enter real blocked TCB states.
Same-domain and cross-domain request/reply do not run under preemption, and the
full sender and receiver death matrix and personality-service crash-containment
gate remain unimplemented. The native `cap_copy` syscall therefore remains
explicitly unavailable. The kernel also does not yet provide an executable
loader, demand paging, SMP, a foreign ABI personality, a root filesystem, the
hosted-broker transport, or a complete IPC system.
