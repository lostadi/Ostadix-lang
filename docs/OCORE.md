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
RAX. Aggregate ABI passing is initially forbidden across extern boundaries;
callers pass pointers instead. The stack is 16-byte aligned before `call`.

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

Inline assembly templates use Intel syntax. Input/output registers are
explicit, implicit clobbers are forbidden, and `options(nostack)` asserts that
RSP is unchanged. Assembly is unsafe even when it contains no privileged
instruction.

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

The first runtime supplies boot entry glue, zeroing `.bss`, serial I/O, IDT
installation, a timer interrupt, page-frame bump allocation, syscall entry,
and panic-to-serial. Allocation in interrupt context is forbidden until a
separate interrupt-safe allocator exists.

## 9. Capabilities and syscalls

A generic or deserialized `OCapability` is not kernel authority. An
`OCapability` emitted by a live hosted broker is a bearer for a private session
binding. Kernel authority itself is represented by an unforgeable
`(slot, generation)` handle tied to a per-process capability table:

```text
CapabilityEntry = { object_id, object_type, rights, generation, occupied }
CapabilityHandle = (generation << 32) | slot
```

Every capability syscall validates slot bounds, occupancy, generation,
object type, and requested rights. Handles never contain kernel pointers.
Closing or transferring a slot increments its generation before reuse.

Initial syscall numbers are:

| Number | Operation |
|---:|---|
| 0 | `debug_write(cap, ptr, len)` |
| 1 | `cap_close(cap)` |
| 2 | `cap_copy(dst_process, cap, rights)` |
| 3 | `page_alloc(memory_cap, count, flags)` |
| 4 | `yield()` |

The hosted `OCapability` wire value may refer to a live kernel capability only
through an authenticated transport endpoint. Its string `identity` is never
accepted directly as a kernel handle.

The hosted `CapabilityBroker` implements this boundary. It generates 256-bit
bearer identities from operating-system entropy, keeps a private per-session
token-to-handle table, verifies capability kind and rights before transport,
and forwards only the bound generation-tagged u64 handle to a
`KernelSyscallTransport`. Deserialized identities not already bound in that
live broker session are rejected as forged or stale. Metadata cannot select a
slot or add rights.

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

## 11. Implemented v0.1 boundary

The initial compiler targets only `x86_64-unknown-none` and uses a simple
stack-spill backend without optimization or register allocation. Direct calls
are supported; function-pointer types are representable, but indirect calls
are not yet lowered. Aggregates support layout, construction, fields,
indexing, locals, statics, and copies, while aggregate parameters and returns
must currently be passed through pointers. Enum construction is supported;
pattern matching is not. Floating-point types reserve their ABI layouts but
floating-point computation is not implemented.

The kernel proof uses a physical-page bump allocator and an identity-mapped
bootstrap address space. It demonstrates a checked kernel syscall dispatcher
and a hosted capability bridge, but not a ring-3 transition or an architectural
`syscall` entry stub. Those are extensions of the specified ABI rather than
dependencies of the boot proof.
