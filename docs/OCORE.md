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

The primary target is `x86_64-unknown-none`. G2 also provides a bounded
`aarch64-unknown-none` scalar backend. Both emit little-endian ELF64 and use an
LP64 data model. The x86_64 default calling convention is System V AMD64; the
AArch64 compiler-versioned `extern "ocore"` convention uses scalar AAPCS64
register placement. `extern "sysv64"` remains AMD64-only and is rejected by the
AArch64 backend.

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
| 2 | `cap_copy(source_cap, destination_endpoint_cap, rights)`; creates an attenuation-only transfer ticket in the M3 scheduler gate |
| 3 | `page_alloc(page_pool_cap, kind)`; returns a generated memory capability |
| 4 | `yield()` |
| 5 | `ticks()` |
| 6 | `exit(status)`; enabled only by a trusted lifecycle harness |
| 7 | `sleep(delta_ticks)`; enabled only while the scheduler is active |
| 8 | `endpoint_create()`; returns a generated endpoint capability |
| 9 | `endpoint_send(endpoint_cap, word0, correlation, transfer_ticket)` |
| 10 | `endpoint_receive(endpoint_cap, message_ptr, 32)` |
| 11 | `endpoint_cancel(endpoint_cap, correlation)` |
| 12 | `serial_read(control_cap, byte_ptr, 1)`; nonblocking and mode-gated |
| 13 | `control_submit(control_cap, command_ptr, len)`; bounded to 192 bytes |
| 14 | `personality_call(call_cap, operation, scalar, timeout_ticks)`; M6A scalar route |
| 15 | `personality_reply(reply_cap, request, status, scalar)`; M6A daemon completion |
| 16 | `personality_supervise(supervise_cap, action, generation, subject)`; M6A policy action |
| 17 | `personality_bounded_call(call_cap, operation, ptr, len, direction, timeout_ticks)`; mode-24 request-scoped bounded-copy route |
| 18 | `personality_view_lookup(reply_cap, request)`; mode-24 daemon view discovery |
| 19 | `personality_view_read(view_cap, offset)`; mode-24 byte read |
| 20 | `personality_view_write(view_cap, offset, byte)`; mode-24 byte write |
| 21 | `personality_bounded_reply(reply_cap, request, view_cap, committed_len, result)`; mode-24 completion |

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
and returns `ERR_NOT_IMPLEMENTED` when the scheduler is inactive. In the M3
gate, `cap_copy` validates an endpoint to derive its receiver CSpace, then
creates an attenuation ticket bound to the exact creating process generation
and that destination CSpace. It does not bind the endpoint object, and the
receiver's CSpace slot remains kernel-selected. The public gate exhausts all 16
ticket records, denies abort by a non-owner process, lets the owner abort each
once, rejects a stale repeat, and proves a fresh prepare succeeds afterward.
Endpoint send/receive/cancel are public CPL3 operations with bounded copy and
real scheduler block/wake behavior. The M5 serial operations require the exact
typed control object installed only in the loaded REPL CSpace.

Mode 18 adds three M6A-only calls: scalar personality call, personality reply,
and supervision. A generation-bound call capability routes the test
personality's scalar operation and input through a kernel request record to an
endpoint owned by the current unprivileged daemon. A typed reply capability is
installed only in that daemon; a rotating typed supervision capability is
installed only in the unprivileged supervisor. The request record arbitrates
reply, supervisor cancellation, deadline expiry, and service death into one
terminal state and one dependent wake. The test personality admits only its
small scalar syscall whitelist; pointer-bearing endpoint operations return
`ERR_NOT_IMPLEMENTED` rather than bypassing a future memory-view protocol.

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

### 10.1 Operator CLI and boot boundary

The repository-owned lowercase CLI exposes the authoritative build and QEMU
paths without reimplementing them:

```text
o kernel doctor
o kernel build
o kernel image
o kernel smoke
o kernel boot
o kernel console
o kernel smoke-live
o kernel gates
o kernel doctor-media
o kernel media
o kernel inspect-media
o kernel boot-media
o kernel smoke-media
o kernel iso
o kernel inspect-iso
o kernel boot-iso
o kernel smoke-iso
```

`boot` selects the baseline probe mode. `console` selects mode 16, builds the
content-addressed M5 service image, prints its exact SHA-256, and boots the real
capability-gated CPL3 `o> ` control process. Its implemented interactive
surface is `status`, exact-digest `install`, and exact-digest `activate`.
The launcher does not promote the parser's reserved or incomplete command
spellings into supported behavior.

Interactive images are loaded by QEMU and use a multiplexed serial terminal;
`Ctrl-A X` exits. `smoke` and `smoke-live` are finite asserted alternatives.
`gates` executes the manifest-defined portable evidence set. These commands
prove only their documented QEMU/TCG boundaries, not a physical-machine boot,
SMP, Linux or Plan 9 boot, or hardware-device isolation.

The deterministic x86_64 GPT/UEFI disk path, separate ISO9660/El Torito UEFI
path, and guarded external-media workflow are documented in
[OSTADIX_BOOT.md](OSTADIX_BOOT.md). The disk and ISO smoke gates rebuild their
respective containers twice and boot the exact admitted read-only artifact
through OVMF/QEMU TCG. They are not physical-machine, KVM, SMP, Secure Boot,
measured-boot, hardware-driver, or hardware-isolation evidence. Only the raw
GPT/ESP disk image is accepted by the external-media writer; the ISO is not.

## 11. Implemented bounded O-core milestone boundary

The broad compiler/kernel target remains `x86_64-unknown-none`; G2 adds a
separate conservative scalar `aarch64-unknown-none` stack-spill backend. Neither
backend has optimization or a general register allocator. Direct calls
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

The bounded AArch64 backend emits statics, scalar locals and pointers,
loads/stores/casts, integer operations, direct calls with at most eight scalar
arguments, branches and current MIR phi shapes, volatile MIR memory, DAIF
masks, `wfi`, and `syscall0` through `syscall6` using x8 and x0--x5. It traps
explicitly on division by zero and signed 64-bit MIN/-1 instead of accepting
AArch64's silent architectural result. It rejects AMD64 `sysv64`, port I/O,
atomics, page invalidation, inline assembly, interrupt/naked functions,
floating point, and wider call shapes.

`ocore/kernel/smoke-aarch64-g2-qemu.sh` builds one deterministic `EM_AARCH64`
image twice, proves semantic markers originate in compiled `.oc`, and boots it
with one vCPU on QEMU/TCG `virt,virtualization=on,gic-version=3`. The image
installs a resident EL2 vector and stack, enters O-core at host EL1, and proves
one domain-separated HVC/ERET round trip with sentinel-register and stack
integrity. That HVC is a host-EL1 bring-up probe, not a guest hypercall or
paravirtual authority interface. It then executes two EL0 principals through real SVC/ERET, endpoint
request/reply, attenuated capability use, contained fault and exit, generation
reuse/stale denial, reclamation, and bounded post-lifecycle architectural
counter progress. It has no stage-2 mappings and remains MMU-off virtual
evidence, not physical AArch64, SMP/G3, KVM/SVM, Linux or Plan 9 boot, a general
foreign ABI, or PCI/DMA/IOMMU/device-assignment evidence.

The future host-EL1-to-EL2 resource boundary, including per-resource
asynchronous completion, memory teardown ordering, G7's no-guest-HVC decision,
and the deferred G8 handle-authentication decision, is specified in
[`O_MACHINE_CONTRACT.md`](O_MACHINE_CONTRACT.md). It is a design contract, not
an implemented G2 claim.

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

Milestone 3 has a bounded public IPC gate. The original kernel-mechanism
regression remains in `smoke-ipc-foundation-qemu.sh`; `smoke-ipc-qemu.sh` adds
CPL3 endpoint create/send/receive/cancel, real full-queue TCB blocking and
wake-once retry, preemptive cross-domain request/reply, exact attenuated
capability transfer, automatic dead-sender cleanup, and containment of a
deliberately crashing personality while unrelated worlds progress. The gate
then tears down every process and capability generation, reclaims all frames,
and reaches a later timer. It is fixed-capacity and single-CPU and does not
claim every death/cancellation interleaving or the request-scoped foreign-memory
protocol.

Milestone 4 adds a deterministic read-only OVFS importer, freestanding SHA-256
verification, strict static x86_64 ELF validation, BSS and minimal SysV stack
materialization, loaded W^X address spaces, and capability-returning service
lookup. `smoke-loader-qemu.sh` executes two independently linked personalities
at the same virtual addresses in different CR3 roots, rejects malformed,
overlapping, and W+X images, then proves transactional namespace teardown and
reclamation.

Milestone 5 is a bounded native live-system slice. Four separately linked
`init`, supervisor, package-daemon, and REPL ELFs load from one immutable OVFS
image into distinct CSpaces. The REPL owns the sole typed serial/control
capability and performs real line collection; privileged fixed-capacity package
and supervisor state machines enforce exact manifests, grants, health-gated
publication, stale generations, rollback semantics, and targeted restart. In
mode 16, `smoke-live-qemu.sh` drives the real serial install/activate lifecycle,
contains one package-daemon CPL3 fault, withdraws the old service in
`CONTROL_RECOVERING`, health-gates a fresh loaded generation before republication,
and proves final deactivation. Mode 17's `smoke-live-semantics-qemu.sh`
independently executes the broader finite state corpus. These gates do not claim
general retry/backoff, recovery from a replacement fault, unprivileged
endpoint-backed service policy, durable reboot reconstruction, a native
compiler, a dynamic linker, a foreign ABI personality, or a foreign root
filesystem.

M6A is a separate bounded personality-supervision slice in mode 18.
`build-m6-artifacts.sh` deterministically packages a test client, native
personality daemon, native supervisor daemon, and unrelated observer at exact
`/sbin/m6-*.elf` paths in a 62,104-byte immutable OVFS image. Its SHA-256 is
`f5924eeb64b5a3d332e20b5d0fae7b233ae2714eb58b72ea07f08a4d26334417`.
`smoke-personality-qemu.sh` checks the artifact identity and absence of the four
user modules from kernel symbols before boot. In QEMU, the CPL3 supervisor
performs health-before-publication, cancels one held request, observes the
daemon-owned endpoint close after a deliberate daemon fault, requests one
fresh generation, health-gates republication, and requests cooperative stop.
The client proves the pinned ping/add-one/unsupported scalar corpus, timeout and
crash results, a rotated call capability, denial of the stale generation-1
capability, and the generation-2 corpus. Late cancelled, late timed-out,
prior-generation, and duplicate replies are rejected. The router retains a
16-record bounded terminal history and the gate requires history count 9 with
zero eviction; an ancient record outside that horizon would still be denied but
classified conservatively as stale. The supervisor queues the fault watch before
cancellation wakes the client, so endpoint FIFO orders the watch ahead of the
timeout/crash sequence. An unrelated observer continues; all resources are
reclaimed; and a later timer remains live.

This evidence is deliberately named M6A, not full Milestone 6. It has no shared
or request-scoped foreign-process memory view, no pointer-bearing personality
call, no general package dependency resolver or durable reconstruction, no
unbounded retry policy, and no Linux or other foreign operating-system ABI.

M6B's first native mechanism slice is separate mode 19.
`personality_memory_view.oc` uses four fixed-capacity generation-tagged request
views, kernel-owned bounded-copy staging, direction-attenuated nontransferable
capabilities, snapshot input, and written-prefix-only output commit after exact
process/address-space revalidation. View size is capped at 128 bytes and total
charged staging at 256 bytes. Reply, cancellation, timeout, service-death,
process-exit, unmap, and delegated-resource hooks close the service capability
before recording one terminal result and publishing one wake. Process-exit and
unmap hooks also release an undeliverable replied view without publishing a
second result or wake. `delegated_resource.oc` supplies independently revocable
typed leases for memory, filesystem, timer, network, and device classes.
Lease/view binding is exact-request-only, and request-wide revocation leaves
unrelated requests live without ambient fallback. Create-plus-bind is an
authority-publication transaction: an injected bind failure must revoke the
new capability and destroy the exact unbound lease generation.

`smoke-m6b-qemu.sh` exercises that mechanism against a real kernel process and
address space, including bounds/generation/mapping-rights/quota denial, staged
commit, stale/duplicate authority, every implemented terminal hook,
same-request bulk revocation, unrelated-request survival, post-reply cleanup,
CSpace-drain close, and a later timer. Lifecycle and wake hooks are invoked
directly; this is not real process/unmap/scheduler integration. It is not routed
through the M6A CPL3 daemon and does not add a public pointer-bearing
personality call.
Pinned windows, streaming output, actual signal and concurrent mapping-change
integration, Linux-oracle behavior, fuzzing, allocation-failure injection, and
concrete filesystem/network/timer/device services remain future M6B work.

Mode 24 is a bounded M6B vertical slice. It composes the mode-19 view
mechanism with the existing M6A request router and four independently packaged
CPL3 principals. `build-m6b-live-artifacts.sh` deterministically rebuilds a
client, personality daemon, supervisor, and unrelated observer into a
65,152-byte OVFS image whose SHA-256 is
`5b9d2526da2abd75ec90b4770ded5923d856132fad736fb13f241c34f1579887`.
The client issues each call once through syscall 17; the live corpus uses one
exact four-byte `INOUT` view. The daemon discovers only the view correlated to
the received request, accesses it through syscalls 18-20, and completes it
through syscall 21. A generation-1 cancellation and deliberate daemon fault
are followed by a health-gated generation-2 rebind. Generation 2 then covers
supervisor-triggered pre-terminal unmap, request-revoke, delegated-device-
resource-revoke, and caller-exit dispositions while each request is still
waiting. The caller-exit case also leaves the test client's process reapable
and its thread exited; the unrelated observer and a later timer survive.

Those lifecycle operations are bounded dispositions, not a claim that the
gate mutates a mapping or observes an external resource event. The delegated
device resource is one internal typed lease, not a physical device. Mode 24
does not cover a post-reply/pre-consume process-exit or unmap race, pinned
windows, streaming, signals, a general foreign ABI, Linux or Plan 9 boot, a
general guest agent, KVM, PCI, DMA, IOMMU, or physical-device isolation.

Run the live bounded personality evidence gate with:

```bash
./ocore/kernel/smoke-live-bounded-personality-qemu.sh
```

Mode 25 is a bounded execution-and-personality vertical slice. It reuses the
Mode 24 bounded request/view terminal path but admits one exact Linux x86-64
personality instead of broadening the native test ABI. A deterministic builder
packages exactly `/bin/linux-minimal.elf`, `/sbin/linux-personalityd.elf`,
`/sbin/linux-supervisord.elf`, and `/sbin/linux-observer.elf` into one immutable
OVFS image. The foreign payload is independently pinned at 8,520 bytes with
SHA-256
`06240b6a840ed4262835aceff64a94f6ebd77838666f05eb7415d9a0d1b5868d`;
the complete image is 60,104 bytes with SHA-256
`b380e5cbbe50403bd58bdafb11c54f2201f0cc742fc898487fa08ba26e2886e8`.
The live gate pins both identities and rejects any of those packaged principals
linked into the kernel as source modules.

The loaded foreign ELF executes at CPL3. Its required path invokes two bounded
`write` calls, one deliberately unknown syscall, and `exit_group(42)`; a fifth
static syscall site is only the failure path's `exit_group(111)`. The write
bridge snapshots at most 128 bytes through one request-scoped `IN` view,
authorizes fd 1 or fd 2 against the exact caller and personality-service
generation, and installs the terminal result directly into saved `RAX` without
reissuing the syscall. The QEMU transcript must contain each exact stdout and
stderr line once, observe `-ENOSYS`, and preserve exit status 42. Generation 1
completes stdout and then faults deliberately. Its view capability is already
closed by the successful reply; crash handling withdraws generation-1 service
and fd authority while preserving the committed terminal record and its charge
until the client can consume it after generation-2 publication. Generation 2
first rejects generation-1 lookup authority as stale during private startup,
then answers health and is published. The client subsequently consumes the
preserved stdout terminal and completes stderr. The gate also keeps the
unrelated observer schedulable, reclaims every request, view, fd object,
capability, process, address space, and frame, and reaches a later timer while
QEMU remains alive.

Run the live Linux-personality evidence gate with:

```bash
./ocore/kernel/smoke-live-linux-personality-qemu.sh
```

Mode 25 executes a Linux-ABI ELF; it does not boot Linux, Plan 9, firmware, or
a distribution and does not provide a dynamic loader, root filesystem, general
Linux ABI, guest agent, KVM or physical-hardware evidence, PCI/device
assignment, DMA isolation, IOMMU isolation, interrupt remapping, or hardware
reset. The exact ELF still awaits an authoritative replay on native x86-64
Linux. This bounded-copy gate also does not close the broader pinned-window,
streaming, signal, SMP, or general concurrent mapping-race M7 acceptance
matrix.

Mode 26 is a bounded execution-and-service vertical slice. It retains Mode 25's
exact 8,520-byte Linux x86-64 ELF and adds an unprivileged native 9P2000 server,
native supervisor, and independently linked native Plan-9-style client. The
deterministic builder packages exactly `/bin/linux-minimal.elf`,
`/sbin/linux-9pd.elf`, `/sbin/linux-supervisord.elf`, and
`/bin/plan9-namespace-client.elf` into a 92,872-byte OVFS image with SHA-256
`920b014cfb133f033b6761da6fe5b1d22be613bf88112c05ec0af982e1beebd9`.
The builder double-compiles each native principal, rejects dynamic or W+X ELF
profiles, double-packs the image, and runs an independent exact-wire oracle
before the image is admitted.

The Linux ELF again completes the exact stdout and stderr writes through
generation-scoped bounded views. The 9P server snapshots only the corresponding
20-byte result and exposes it at `/srv/linux/status`; the Plan-9-style client
receives only a generation-bound bounded-call capability. With `msize = 128`,
it executes exact 9P2000 version, attach, walk, open, read, and clunk messages.
Generation 1 also proves sequence, unsupported-version, missing-path,
write-open, and excessive-count errors. After the first read and clunk, the
server faults deliberately. O-core withdraws the generation-1 namespace and
call authority, preserves both clients, installs and health-publishes a fresh
server, and denies the stale generation-1 client capability. Generation 2 then
serves the exact stderr snapshot before the Linux `-ENOSYS`, `exit_group(42)`,
supervisor stop, complete reclamation, and later-timer checks finish.

Run the live Linux-to-9P evidence gate with:

```bash
./ocore/kernel/smoke-live-linux-plan9-qemu.sh
```

This is real 9P2000 wire behavior between bounded native CPL3 service
principals; it is not a Plan 9 kernel or binary. Mode 26 does not boot Linux or
Plan 9, provide a distribution, root filesystem, dynamic linker, general Linux
ABI, general 9P server, Plan 9 namespace or mount environment, network
transport, persistent filesystem, or guest-agent framework. QEMU TCG is not
KVM/SVM or physical-hardware evidence. PCI or physical-device assignment, DMA,
IOMMU, interrupt remapping, and hardware reset remain outside this gate.

Mode 26 is not two-provider routing evidence. Generation 2 is a replacement
instance of the same server implementation and serves a later, different
20-byte snapshot after generation 1's read and clunk have completed. There is
no route set for one immutable object, requester-local provider choice, recovery
of one logical read on a second provider, fresh provider-B session/fid
reconstruction, causal multi-attempt trace, or live `OWRECEIPT` emission.

Mode 31 is the bounded M7B-1 mechanism gate. Run it with:

```bash
./ocore/kernel/smoke-m7b-logical-read-qemu.sh
```

The deterministic 78,304-byte OVFS contains one requester-local client/router
ELF, one provider ELF instantiated as two distinct generation-bound CPL3
provider principals, and one exact 20-byte immutable object. Before the request,
O-core admits distinct A/B provider identities, service bindings, endpoints,
and client call capabilities. Provider B's service loop remains staged while A
receives fresh `version`, `attach`, `walk`, `open`, and `read` exchanges. A
returns a valid 9P `Rerror`, faults deliberately, and has its local route and
call authority withdrawn; the client then proves its old A capability stale.
Only afterward does B run a fresh provider-local `version`, `attach`, `walk`,
`open`, `read`, and `clunk` sequence with different fids. The client verifies
the exact bytes and SHA-256, and the kernel checks a volatile causal state before
separately proving A physical/process cleanup, B session/queue cleanup, complete
bounded resource reclamation, unrelated-witness survival, and a later timer.

This passes M7B-1, not the complete M7B milestone described in
[`ODOMAIN_PLAN.md`](ODOMAIN_PLAN.md#m7b-two-provider-immutable-9p-read-fallback).
The requester and route coordinator are one principal, both providers use the
same deterministic implementation artifact, and the route set is fixed local
configuration. The causal state is non-persisted, unsigned diagnostic evidence,
not a live `OWRECEIPT`. Mode 31 does not establish implementation-diverse
providers, general 9P or WorldFS, writes, fid migration, exactly-once effects,
network transport, Governor consensus, G7/G8, a foreign kernel, Linux or Plan 9
boot, hardware virtualization, physical-device assignment, DMA/IOMMU isolation,
or physical-hardware evidence.

Mode 27 is the bounded shared-World-identity PR2 slice. The 20 constitutional
identity atoms--`WorldId`, `WorldEpoch`, `GovernorTerm`, `GovernorLogIndex`,
`NodeId`, `NodeGeneration`, `DomainId`, `DomainGeneration`, `ProcessId`,
`ProcessGeneration`, `ResourceId`, `ResourceGeneration`, `ObjectId`,
`ObjectVersion`, `CapabilityId`, `LeaseId`, `TaskId`, `AttemptGeneration`,
`CheckpointId`, and `ReceiptId`--have matching typed Rust and `.oc`
definitions. The gate admits only strict `OWIDENT` v1 identity records and
requires byte-exact Rust/native O-core convergence under QEMU TCG. Strict
decoding rejects malformed records and zero generation/version/term/index
fields; separate hierarchical current/reference checks reject stale
generations and same-generation logical mismatches.
Current `.oc` aggregate fields are constructible, so native validity is
enforced by the nominal initializers and strict record boundaries rather than
claimed as an unrepresentable raw bit pattern. A raw zero or mistagged
aggregate is not an accepted identity record.

Run the native World-identity evidence gate with:

```bash
./ocore/kernel/smoke-world-identity-qemu.sh
```

Serialized capability IDs remain descriptive non-authority: they are not
bearers, CSpace handles, or delegation. `OWIDENT` v1 remains the identity-only
nested format rather than a transport, OValue envelope, or receipt codec. The
gate supplies no Governor, consensus, native membership, or OSTADIX Alpha
qualification; it passes no G0--G13 gate, and QEMU TCG is not physical or
hardware-isolation evidence.

Mode 28 is the bounded canonical World wire-codec PR3 slice. `OWPROTO` v1 uses
a fixed 16-byte big-endian header, four record kinds, a 16 KiB hard maximum,
and caller/negotiated record limits. Strict decoding requires exact total and
payload lengths, known kinds and schemas, zero reserved fields, valid bounded
schema ranges, and canonical nested `OWIDENT` records. The fixed corpus is
exactly 20 records and 1254 bytes: two offers, one canonical v1 selection, one
disjoint rejection, and all 16 identity-v1 conformance records. Rust and native
`.oc` must produce those bytes exactly under QEMU TCG.

Run the native World-protocol evidence gate with:

```bash
./ocore/kernel/smoke-world-protocol-qemu.sh
```

Schema negotiation is an offline deterministic function over two bounded
offers. It chooses the highest common version and smaller maximum-record limit,
or one exact contextual no-overlap rejection; validation rejects downgrades,
inflated limits, and false rejection. It does not open a stream or network
transport, perform a live handshake, authenticate a peer, establish a session,
provide encryption or replay protection, or carry membership. Decoded identity
and capability descriptions remain non-authority and cannot create bearers,
CSpace handles, or delegation. Mode 28 implements neither PR4 OValues nor PR5
receipts, and supplies no Governor, consensus, WorldFS, Workstream A acceptance,
or G0--G13 qualification. QEMU TCG is not physical or hardware-isolation
evidence.

Mode 29 is the bounded canonical World-value PR4 slice. Its separate
self-framed `OWVALUE` v1 format is not a new `OWPROTO` v1 kind. It admits only
the frozen portable core, uses a 4096-byte record maximum with depth limited to
16 and total nodes limited to 128, orders record fields and scalar-key maps
canonically, and permits a root-only inert versioned extension whose payload
must itself be a portable value. SHA-256 covers the complete canonical record.
The fixed corpus is exactly 19 records and 928 bytes (1856 lowercase hex
digits), with concatenated SHA-256
`264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc`. It must
encode identically and hash identically in Rust and native `.oc`; strict decoding and reencoding reject malformed, duplicate,
out-of-order, over-limit, or otherwise noncanonical records.

Run the native World-value evidence gate with:

```bash
./ocore/kernel/smoke-world-value-qemu.sh
```

Hosted conversion is an explicit allowlist. Capabilities, capsules, live
references, requests, and other authority-bearing or effectful hosted values
are rejected rather than serialized. Descriptive code and object references do
not become authority, and extensions never auto-dispatch or rehydrate a
capsule. Mode 29 remains an offline codec/hash oracle rather than transport, a
live M9 crossing, authenticated authority, PR5 receipts, execution/grounding
convergence, a Governor, consensus, WorldFS, Workstream A acceptance, or
G0--G13 qualification. It does not make the full hosted `OValue` enum portable
or replace the hosted canonical-CBOR shim format. QEMU TCG is not physical or
hardware-isolation evidence.

Mode 30 is the bounded canonical World-receipt PR5 slice. Its separate
self-framed `OWRECEIPT` v1 format binds bounded descriptive World identities
and generations, SHA-256 content references, capability-right descriptions,
terminal and commit fields, evidence-gate identity, and an algorithm-tagged
signature envelope. Rust and native `.oc` produce the same fixed two-record,
3,239-byte corpus (6,478 lowercase hex digits; SHA-256
`1edd90bf881cd42d08e2031482baae4e7c9a95bd78cfa65f0cbe14147c0a2604`) and
the same 1,575-byte current and 1,546-byte stale signing preimages. Both strictly
reject malformed or noncanonical record structure.

Run the native World-receipt evidence gate with:

```bash
./ocore/kernel/smoke-world-receipt-qemu.sh
```

Hosted Rust performs real Ed25519 sign/verify, tamper rejection, and wrong-key
rejection using a pinned, explicitly non-secret conformance key. Native Mode 30
validates the receipt and signature-envelope structure but is not a general
freestanding Ed25519 verifier. Capability identities and rights in a receipt
remain descriptive rather than bearer authority. The offline corpus is not a
live execution receipt emitted by HGraph, project, live-system, KernelWorld,
O-Git, or evidence components. It supplies no production key lifecycle,
trusted-signer policy, authoritative replay/commit fencing, transport,
Governor, consensus, WorldFS, typed OSTADIX Alpha attestation, Workstream A
acceptance, or G0--G13 qualification. QEMU TCG is not physical or
hardware-isolation evidence.

Mode 32 is the bounded native semantic-structure comparison for a live
hosted-reference project receipt. Its serial oracle accepts one lowercase-hex
OWRECEIPT of at most 4,096 bytes, performs the complete native canonical
decode, exact envelope reencode, and validated signing-preimage construction,
then requires the receipt's commit tag to be `Uncommitted`. Native and hosted
Rust compare
`SHA256("OSTADIX/PROJECT-RECEIPT-SEMANTICS/V1\0" || u32_be(body_length) || canonical_unsigned_body)`;
the signer key ID and signature remain outside that semantic fingerprint.
After the successful comparison, the probe reuses the same receipt-validation
scratch with a malformed envelope and requires
`validated_terminal_and_commit` to fail, proving that early rejection cleared
the prior success-only terminal and commit tags.

The required release gate is the no-argument wrapper. It runs the focused
hosted World-project test to generate a fresh receipt and semantic digest, then
invokes the direct two-argument vector interface shown below it:

```bash
./ocore/kernel/smoke-world-project-runtime-qemu.sh
./ocore/kernel/smoke-world-project-receipt-qemu.sh \
  RECEIPT_HEX_FILE EXPECTED_SEMANTIC_SHA256
```

Mode 32 proves native canonical receipt interpretation and unsigned-body
semantic equality for that bounded record plus fail-closed scratch-tag reset.
The hosted receipt's placement is a caller-supplied coordinator observer, not
the deployment's proposed provider; its attempt is a dedicated coordinator
attempt rather than a route attempt, and its subject leaves package absent
instead of storing the provider implementation. Mode 32 does not independently
validate those hosted architectural choices. It does not execute the project in
O-core, verify Ed25519 natively, establish signer trust, authorize residual
`HostWorld` effects, change the `Uncommitted` fence, or supply Governor
admission/commit, authenticated membership, provider ownership or placement,
capability/lease authority, reservation, remote dispatch, consensus, recovery,
exactly-once execution, OSTADIX Alpha acceptance, a G0--G13 gate,
physical-hardware evidence, or hardware-isolation evidence.

Mode 20 is a separate bounded KernelWorld supervisor-admission and object-model
gate. A host-side `VerifiedKernelWorld` produces a deterministic `OKWORLD1` V2
normal form that keeps verified package and canonical manifest digests
distinct. The current fixture is exactly 459 bytes with SHA-256
`0ece5f7f37ebe203d03cc7e5213dc8f9257a9a225a73e52d37d1f718424b9232`
and exactly the canonical backend requirements `["npt", "svm"]`. The kernel
verifies that exact record hash before strict parsing. It requires unique
request kinds and binds every device export's explicit `authority_request` to
one exact existing `device.*` request; non-device exports omit the field.
Several exports may share a request, while `max_devices` charges distinct bound
device authorities. Reserved rights are typed: `vm.machine` admits only
`run|stop`, and `device.*` admits only `reset|dma`.

Mode 20 admits at most two worlds through independently registered default-deny
policy keyed by package digest and copied exact request-kind/purpose bytes;
hashes are only a fast reject and never authority. Its nonexecuting VM model
supports at most two VM identities, four vCPU identities, and eight aligned
guest-page attachments backed by anonymous 4 KiB memory objects. The gate proves
exact export-authority and typed-right denial, quota and overlap denial,
generation checks, exact-world revoke/reclaim, an unrelated surviving VM, and a
later timer. The local VM graph may be sealed, but package admission
deliberately remains `ADMITTED`; no provider configured or start lifecycle is
claimed.

These are configuration objects, not a hypervisor. Mode 20 does not start or
health-check a provider, publish exports, boot a guest, enter VMX/SVM, construct
EPT/NPT, execute firmware, inject interrupts, assign devices, map DMA, or
configure an IOMMU.

Mode 21 is the separate AMD hardware-execution gate. The host requires KVM plus
the exact `svm` and `npt` CPU features, and the kernel byte-compares the V2
record's complete requirement vector to exactly `["npt", "svm"]` before SVM
initialization. It enters only a two-page real-mode synthetic guest through a
private NPT, proving one bounded interrupt, a controlled `VMMCALL`, denial of
an unmapped GPA, exact NPT teardown, stop/restart, and unrelated-VM survival.

Mode 22 is a separate TCG-compatible native boot-service lifecycle mechanism
gate. `kernel_world_boot.oc` has fixed capacity for two administrative boot
instances and four published exports. It stages an admitted world with one
configured VM identity and one exact consumer CSpace. `start` requires the
independently materialized `vm.machine:run` grant, but is only an
administrative state transition: it does not enter the vCPU or start a process,
guest, or foreign-kernel provider.

Publication requires a trusted observation of the exact health protocol ID
retained from the admitted record. A published export installs a
nontransferable `OBJECT_KERNEL_WORLD_EXPORT` capability directly into that
consumer CSpace. Every export carries a status right. A device-plane export
also carries only a reset-*request* right, and only when its byte-exact sealed
authority request received the independently granted `device.*:reset` right.
Lookup returns that already-installed exact capability and grants no authority.
Name/protocol resolution compares retained IDs together with the exact consumer
CSpace and required-right set; the native record does not retain the original
name/protocol bytes for byte-exact lookup. Authority comes only from the
already-installed capability. Publication denies a second live binding with the
same consumer-CSpace/name/protocol ID tuple.
The status operation reports the native boot generation; reset success means
only that O-core accepted broker intent for the exact live binding. It does not
dispatch to a provider or reset hardware.

Generic `SYS_CAP_CLOSE` recognizes a KernelWorld export and routes it through
the same registry-aware close transition. The binding becomes unavailable,
the capability is closed, and its service generation advances together.
Closing the final export returns that boot from `ACTIVE` to `HEALTHY`, so it can
publish again or proceed through orderly teardown without orphaned live
metadata.

Failure makes the boot generation unavailable, closes and generation-retires
all issued client capabilities, and only then revokes the exact VM graph.
Admission is retained so the declared `on_failure` policy can authorize one
fresh VM/boot/service generation and rebind; stale capabilities remain denied,
and an unrelated live service survives. Explicit stop leaves a terminal
tombstone: only `always` policy can consume it as a restart; otherwise the
owning uninstall transition first revokes admission and then consumes the
tombstone, but only after proving that no active boot or exact local VM graph
remains. A configured, un-staged replacement makes uninstall fail without
changing its graph, ticket, or admission. There is no separate public
tombstone-abandon operation, so a failed admission revoke cannot reopen the old
generation through ordinary staging.
`smoke-kernel-world-live-qemu.sh` exercises these transitions directly under
QEMU TCG and observes a later timer.

Every externally callable lifecycle or broker transition is serialized by one
single-CPU operation owner and completes at a monotonically advancing
linearization epoch. This rejects re-entry in the current gate; it is not an
SMP lock. A future SMP port must replace the ownership byte with an atomic
kernel lock while preserving the same transition boundaries.

Mode 22 does not enforce the declared health timeout, and its failure hook is
called directly by the bounded semantics gate rather than by a process fault,
trap, scheduler, or vCPU-exit path.

Mode 23 is a separate bounded execution-and-device composition gate. It runs
the AMD SVM/NPT architectural path under QEMU TCG: the outer emulator, rather
than KVM or physical AMD hardware, executes the nested guest entry and VMEXIT
instructions. One generation-tagged execution session is bound to the exact
live boot, admitted-world generation, configured VM, current vCPU, code page,
mailbox page, device-plane export ordinal, and independently granted request.
A cross-world vCPU is rejected before either SVM state or a virtual endpoint is
made live. The VM carries one execution pin while SVM owns its page mappings,
so ordinary boot failure, stop, or graph revocation cannot race that ownership.

The fixed real-mode synthetic guest first exits through the exact `VMMCALL`
expected by the coordinator. The guest supplies no health protocol identifier:
the coordinator derives the retained protocol from the bound admitted world
and only then records health and permits publication. The next guest phase
executes a non-string, non-`REP`, 32-bit `OUT` to port `0xE0`. O-core validates
the complete IOIO exit and dispatches its scalar value only to the
generation-bound kernel-internal endpoint for that session. The sole operation
returns `input XOR 0xA5A55A5A`; the broker disposition is placed in guest RAX
before execution advances beyond the intercepted instruction.

Mode 23 also connects the published reset-request capability to the exact live
virtual endpoint. Reset clears only that endpoint's scalar transaction state
and leaves the assignment owned by its execution session; it is not physical
device reset. An exact nested-page fault is the bounded failure notification.
The coordinator records the NPF, disables SVM and clears NPT while releasing
the execution pin and retained mappings, revokes the virtual endpoint, and
then invokes the boot terminal transition. That transition withdraws the
client capability before revoking the exact VM graph. An unrelated published
service remains usable. The declared `on_failure` policy then creates fresh
generation-2 VM, boot, execution-session, endpoint, and client identities;
the generation-1 session and capability remain stale while the replacement
repeats health and device execution. Orderly stop and uninstall reclaim both
worlds and a later timer remains live.

Run the portable evidence gate with:

```bash
./ocore/kernel/smoke-kernel-world-execution-device-qemu.sh
```

Mode 23 does not boot Linux, Plan 9, firmware, or a supplied user image; its
guest program is the fixed synthetic two-page probe. It provides no general
guest agent, shared queue or ring, asynchronous request processing, or SMP
coordination. Its virtual PIO endpoint is not PCI or physical-device
assignment, DMA, an IOMMU boundary, interrupt remapping, or hardware reset.
The TCG gate is not KVM evidence and establishes no physical-hardware
isolation. Modes 21 and 23 enter the synthetic guest for their separately
scoped gates; Mode 22 deliberately does not enter any guest.
