# O-Domain Engineering Plan

Status: active roadmap. Milestones 0.1 through 5, the bounded M6A/M6B slices
including Mode 24's live four-byte composition, Mode 25's exact static-Linux
ELF/minimal-ABI slice, Mode 26's exact Linux-to-9P2000 service composition, and
KernelWorld Modes 20 through 23 now have executable evidence at their
documented, fixed-capacity, single-CPU x86-64 QEMU boundaries. These are
bounded mechanism, lifecycle, and synthetic-execution proofs, not
production-scale implementations or evidence for a general foreign
operating-system ABI.

This document turns the poly-personality kernel brief into a dependency-ordered
implementation plan for this repository. It is a claim boundary as well as a
roadmap. Items described as planned are not implemented merely because their
types, syscall numbers, setup scripts, or names already exist in source.

The separate native distributed-fabric constitution is
[`OSTADIX_WORLD.md`](OSTADIX_WORLD.md), with qualification rules in
[`world_alpha_gates.toml`](../evidence/world_alpha_gates.toml). An O-Domain may
later run on a World node, but the bounded native gates in this roadmap do not
establish a replicated Governor, native node membership, WorldFS, physical
resource registry, or distributed placement layer.

The proposed additive split between node-local O-Machine/O-core authority,
requester-local route selection, and replicated global facts is recorded in
[`LOCAL_AUTHORITY_ROUTING_AMENDMENT.md`](LOCAL_AUTHORITY_ROUTING_AMENDMENT.md).
That clarification does not alter the sealed contracts and is not an
implementation or evidence claim.

## 1. Scope and terminology

O-core remains the small privileged mechanism layer. It owns CPU entry,
interrupts, memory protection, scheduling, IPC, capability validation, timers,
and delegated device resources. A compatibility personality must not become an
unrestricted extension of the privileged kernel.

An **O-Domain** is one persistent user-space world. It is an instance, not an
ABI implementation. Its eventual record contains:

- a generation-tagged domain identity and lifecycle state;
- one personality and architecture;
- one root filesystem and mount namespace;
- process and service namespaces;
- a set of processes;
- granted capabilities and resource quotas;
- package/environment metadata; and
- explicitly versioned persistent state.

A **personality** implements the application-facing contract for a family of
binaries. It owns executable probing, ABI-specific initial process state,
syscall numbering and semantics, signals or exceptions, and native object
rules. One Linux personality can serve many domains. Debian and Alpine are
different root filesystems and userland compositions, not different Linux
personalities.

A **process** is a schedulable protection container inside one O-Domain. Its
kernel-owned process control block (PCB) identifies its PID, domain,
personality, state, address space, kernel entry stack, user entry and stack,
threads, and capability space. A process is not an O-Domain, and a persistent
hosted evaluator such as `python[0]` is not yet an O-core process.

A **capability space**, or **CSpace**, is the process-local table that resolves
generation-tagged integer handles to kernel objects, types, and rights. The
table is authority. A serialized `OCapability` is not. The hosted
`CapabilityBroker` in `src/ocore/capability_bridge.rs` already models the
required outer boundary by resolving a random session bearer through
operation-specific policy to a live kernel handle before transport, but it is
not yet connected to the booted kernel.

A **root filesystem** supplies the files, libraries, package manager, and
configuration visible in a domain. The scripts under `setup/os/` install and
build O-lang on host operating systems. They are not O-Domain root filesystem
images.

The eventual execution modes are:

1. **translated**: a personality service implements a foreign ABI over O-core
   mechanisms;
2. **user-space kernel**: a bounded service runs substantial foreign kernel
   components and receives only delegated capabilities; and
3. **full kernel**: a subordinate kernel runs in a VM or equally strong
   isolation boundary.

Each mode implements the same stateful, authority-aware personality operation
where its advertised service contract overlaps:

```text
P_L(operation, arguments, state_L, delegated_capabilities)
    -> (result, next_state_L, effects)
```

This common boundary does not flatten native semantics. Linux descriptors, NT
handles, Lisp objects, and Darwin ports remain personality objects. OValues
carry structural data, capabilities carry live authority, and native capsules
preserve objects that cannot be normalized honestly.

CPU architecture emulation is a separate concern. The first personality target
is x86-64 on the existing `x86_64-unknown-none` compiler and kernel path.

## 2. Current repository boundary

Before Milestone 0.1, the O-core vertical slice provided:

- `.oc -> AST -> typed HIR -> SSA MIR -> x86-64 ELF object` through `ocorec`;
- a bootable long-mode kernel image;
- serial output, an IRQ0 timer, and an IDT entry for that timer;
- a physical-page bump allocator with no reclaiming free path;
- generation-tagged capability slots in a small global table; and
- a checked `kernel_syscall_dispatch` implementation for capability-gated
  debug output, although the committed boot path does not enter it through the
  architectural `SYSCALL` instruction.

Milestone 0.1 replaces that last limitation with a narrow, auditable native
user-mode proof. Its boot and linker changes add user GDT selectors, a TSS and
ring-0 entry stack, `EFER.SCE` and `EFER.NXE`,
`STAR`/`LSTAR`/`FMASK`, a CPL3 `IRETQ` entry, and two fixed user mappings:

| Virtual range | Milestone 0.1 use | Intended permissions |
|---|---|---|
| `0x01000000..0x01200000` | linked native user text and read-only data | user, read, execute |
| `0x01200000..0x01400000` | native user stack | user, read, write, NX |

The implementation also supplies a native-only domain registry, one
kernel-owned PCB, a process-local typed CSpace, current-process syscall routing,
bounded user-memory validation, and a linked `native[0]` payload. Separate ELF
load segments reserve and zero-fill the complete RX image and RW stack ranges.
The complete image builds and the QEMU smoke gate passes with the required
negative cases.

Milestone 0.2 replaces the broad supervisor identity mapping and raw
user-pointer dereference with a hardened bootstrap boundary. Kernel metadata,
text, read-only data, and mutable state now have page-granular R/NX, RX, R/NX,
and RW/NX permissions respectively. User and privileged stacks have absent
guard pages. The linked user payload interval is RX, its zero-fill tail is
R/NX, and its stack is RW/NX. A concrete bootstrap address-space descriptor
records the live CR3 and matching region permissions. `SWAPGS` selects
boot-CPU-local entry state, and assembly exception stubs normalize vectors 0
through 31 into a kernel-owned
trap frame. Exact page-fault fixups recover only active user-copy loads and
stores. `debug_write` uses a bounded kernel bounce buffer.

The default QEMU smoke gate proves the nonfatal path. A second gate performs a
fresh boot for each fatal probe: divide error, invalid opcode, canonical
non-present read, supervisor read, guard-stack write, NX instruction fetch,
noncanonical target, and invalid syscall-return RIP. Each accepted probe marks
the only PCB `FAULTED`, clears the current process, abandons the user frame, and
reaches a later kernel timer marker. This is controlled fault disposition for
one process, not evidence of sibling-process isolation.
An additional fresh boot removes one test user-image PTE after the software
region is registered. `debug_write` must recover at the exact copy-load fixup,
return `ERR_USER_COPY_FAULT`, and reach a later CPL3 heartbeat without changing
the PCB to `FAULTED`.

Milestone 0.3 replaces the fixed-window bump pointer with a reclaiming frame
registry over the already mapped supervisor-only 4..16 MiB QEMU bootstrap
window. Frame and memory-object identities are generation-tagged. Frames carry
an explicit kernel, page-table, executable, anonymous, or shared RAM type,
reference count, and free-stack position. Device memory is a distinct rejected
kind rather than RAM that can be allocated accidentally. Final release zeros a
frame before reuse, advances its generation, and retires it rather than wrapping.

`page_alloc` now validates a page-pool capability and requested memory kind,
enforces a per-CSpace frame quota, and returns a freshly selected typed CSpace
capability. The internal memory-object handle and physical frame address do not
cross the ABI. Closing the final memory capability releases its object and
frame. Kernel-side tests exhaust and reclaim all 3,072 managed frames, verify
zeroing and refcounts, reject double and stale release, and roll back a
post-free-stack-pop injected failure. CPL3 tests cover capability validation,
quota denial and recovery, invalid device allocation, stale close, and complete
lifecycle cleanup.

At the Milestone 0.3 acceptance boundary, preparation interfaces were
intentionally narrow. `process::activate_bootstrap` could succeed only once and
could not impersonate a context switch. `address_space::install_bootstrap`
recorded the actual CR3 and fixed regions but exposed no create, map, unmap, or
destroy operation. The scheduler counted yield requests but had no runnable
queue or switch operation. Capability copy retained its stable native ABI
number but returned `ERR_NOT_IMPLEMENTED`. Memory-object capabilities were not
mappings, and no public page-table editing or user virtual-address selection
existed. Milestones 1 and 2 supersede those process and scheduler limits below;
the general user mapping and capability-copy ABI limits remain. This historical
boundary is retained so later milestone names are not projected backward into
the earlier evidence.

## 3. Milestone 0.1: native[0] CPL3 and SYSCALL proof

Status: **complete**.

### Required proof

One statically linked O-core payload must:

1. be registered as process 1 in `native[0]`, the only registered personality
   and domain instance;
2. enter at CPL3 with its fixed user text and stack ranges;
3. observe zero-fill in the reserved image tail and stack segment;
4. execute the x86-64 `SYSCALL` instruction;
5. switch immediately to a trusted ring-0 stack without first using the
   untrusted user stack;
6. route the call through the current PCB and native personality;
7. validate the capability slot, generation, object type, and rights in that
   process's CSpace;
8. validate the complete user pointer range before debug output;
9. return only to an allowed user RIP and RSP with sanitized RFLAGS, including
   a CPL3-set NT flag that would make an unmasked kernel `IRETQ` fault;
10. deny out-of-bounds and empty slots, wrong generations, stale handles after
   live same-slot reuse, wrong rights, wrong types, closed handles, crossing
   and wrapping ranges, kernel pointers, and unknown syscalls;
11. exercise the stable `yield` hook without claiming a scheduler; and
12. observe an IRQ0 transition from CPL3, return through `IRETQ`, and continue
    through a later CPL3 heartbeat.

The asserted serial marker set is:

```text
O-core kernel: serial online
page allocator: online
capability: online
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
RFLAGS sanitization: online
timer CPL3 return: online
yield hook: online
CPL3 heartbeat: online
```

The timer's one-byte `T` is asynchronous, but the gate requires it as a
standalone line before `timer CPL3 return: online`, then requires a later CPL3
heartbeat. Since architectural syscall entry masks IF, the counter used by the
probe can advance only while the loop is back at CPL3. This supplies executable
evidence that the privilege-changing interrupt used the TSS stack and returned,
instead of merely proving that the handler printed before a later failure.

The smoke test must also reject any `LEAKED` payload and require QEMU to remain
alive for the observation window. A marker printed directly by ring 0 is not a
substitute for the CPL3 syscall path that the marker claims to test.

### Accepted honest claim

The maximum defensible claim is:

> On x86-64 QEMU, O-core can enter one linked native O-core payload at CPL3,
> receive its architectural `SYSCALL` on a trusted kernel stack, route it
> through the current native-domain PCB, enforce a process-local
> generation-tagged typed capability and complete bounded user range, reject
> the asserted negative cases, sanitize hostile entry flags, return to the same
> payload, and survive repeated CPL3 timer transitions through a later
> heartbeat.

### Explicit non-claims

Milestone 0.1 does not establish:

- two isolated processes or two concurrently running O-Domains;
- per-process page tables, general virtual memory, demand paging, ASLR, or a
  reclaiming allocator;
- page-granular kernel W^X;
- a context-switching, blocking, preemptive, multicore, or fair scheduler;
- a general process/thread lifecycle, fork, exec, wait, or signals;
- capability transfer between processes, IPC, shared memory, or service
  discovery;
- a filesystem, VFS, mount namespace, root filesystem, or persistent domain;
- an ELF loader, dynamic linker, or execution of an external binary;
- Linux, BSD, NT, Darwin, Lisp-machine, POSIX, or libc compatibility;
- dynamically installable or crash-isolated personality services;
- kernel transport for hosted `OValue` or `OCapability` values;
- general syscall pointer safety beyond the operations and fixed ranges tested;
  or
- production hardening against SMP races, nested entries, hostile exception
  storms, side channels, denial of service, or device DMA.

The name `native[0]` identifies the first domain-shaped instance. It does not
by itself prove the full O-Domain abstraction.

## 4. Dependency-ordered implementation milestones

Each milestone starts only after the previous milestone's acceptance gate is
recorded. A passing build is necessary but is not the runtime gate.

### Milestone 0.2: harden architectural user entry

Status: **complete** for the single-CPU bootstrap boundary.

Implemented complete exception gates for faults originating at CPL3, a
kernel-owned trap frame, full integer-register preservation, bounded
`copy_from_user` and `copy_to_user`, overflow-safe region checks, user and
privileged guard pages, CPU-local entry state, a double-fault IST, and
page-granular kernel RX/R/RW-NX permissions. Global syscall scratch was removed
in favor of a GS-selected boot-CPU record. The design remains single-core; SMP
requires one allocated entry record, copy state, TSS, and scheduler transaction
per CPU.

Acceptance gate:

- deliberate user page faults, divide error, invalid opcode, bad stack, invalid
  RIP, and invalid syscall-return RIP fault the current process and leave the
  kernel timer alive;
- oversized and wrapping buffers return explicit errors while the process
  continues;
- kernel text/data remain inaccessible from CPL3;
- kernel text is RX, kernel read-only data is R/NX, writable kernel state is
  RW/NX;
- faults during user copy return a defined error without corrupting kernel
  state; and
- entry/return tests pass under repeated timer interrupts.

The executable gates are `ocore/kernel/smoke-qemu.sh` and
`ocore/kernel/smoke-faults-qemu.sh`. The fatal matrix starts a fresh VM for each
probe and rejects unexpected vectors, CPL0 faults, early VM exit, or a missing
post-fault timer marker. The accepted claim is controlled disposition of the
only process. Sibling survival remains a Milestone 1 acceptance property.

### Milestone 0.3: reclaiming physical memory and memory objects

Status: **complete** for the fixed single-CPU QEMU bootstrap window.

The allocator tracks frames, reference counts, types, deterministic zeroing,
allocation quotas, generation retirement, and reclaim paths. Kernel,
page-table, executable, anonymous, and shared RAM are distinct allocation types;
device memory is explicitly rejected from the RAM pool. Memory-object handles
are generation-tagged from their first public lifecycle API. No raw registry
slot, physical address, or caller-selected object ID crosses the kernel ABI.

Acceptance gate:

- allocation/exhaustion/reclaim stress returns every non-pinned frame;
- a reallocated frame contains no data from its prior allocation or owner;
- double free and stale memory handles fail closed; and
- allocator invariants survive injected allocation failures.

`ocore/kernel/smoke-qemu.sh` asserts each kernel and CPL3 acceptance marker in
order and requires a later timer return and heartbeat. The Milestone 0.2 fault
matrix remains a separate fresh-boot gate and still passes in full.

This is not a general firmware-discovered physical-memory manager. The managed
window is the fixed 4..16 MiB range already reserved and identity-mapped by the
QEMU bootstrap. It contains no pinned frame, so the exhaustion test must return
all 3,072 frames. Parsing arbitrary firmware maps, reserving modules, MMIO
registration and concurrent allocator locking remain later work. Milestones 1
and 2 subsequently replaced the bootstrap-only mapping/owner boundary with
generation-tagged process, address-space, mapping, CSpace, and thread
lifecycles. Those later results do not widen this fixed-window allocator claim.

### Milestone 1: process model, address spaces, and CSpaces

Status: **complete** for two bounded native processes on one CPU.

Give each process its own top-level page table, VM map, user stacks with guard
pages, kernel entry stack, thread set, state machine, and CSpace. Make domain,
process, address-space, and mapping handles generation-tagged. This transition
must precede public create/destroy/reuse APIs so raw-slot identity cannot spread
through later interfaces. Map kernel pages supervisor-only and apply W^X and NX
at page granularity. Define create, start, stop, exit, reap, and domain teardown
transactions.

Acceptance gate:

- two native processes use the same virtual address for different physical
  pages and cannot observe or modify each other;
- switching CR3 and the current PCB also switches the active CSpace;
- stale process, domain, address-space, mapping, and capability handles are
  rejected; and
- killing one process reclaims its private mappings, stacks, and capability
  references without harming its sibling.

The implementation uses generation-tagged domain, process, address-space,
mapping, and CSpace owner handles. Each dynamic process root maps shared RX
user text, private RW/NX data and guarded stacks, and supervisor-only kernel
pages. Process reap is split into ownership release, address-space destruction,
type-aware CSpace drain, and final generation advance. Probe modes 10 and 11
exercise normal exit and contained user fault respectively. Both reuse the
same private virtual address for distinct physical pages, tear down process 1,
reject its stale identities after same-slot reuse, and prove process 2 still
runs before reclaiming every dynamic frame.

`ocore/kernel/smoke-processes-qemu.sh` is the executable acceptance gate. It
boots both scenarios independently, checks ordered lifecycle markers, rejects
fault/leak output outside the selected scenario, requires sibling survival,
and observes a post-lifecycle timer.

This result does not provide demand paging, copy-on-write, ASLR, arbitrary user
mapping selection, fork/exec/wait, signals, SMP, or a general process service.

### Milestone 2: threads and scheduler

Status: **complete** for four TCBs, two processes, and one CPU.

Add saved thread contexts, runnable and blocked queues, timer-driven preemption,
cooperative yield, sleep/timers, wakeup reasons, priorities, accounting, and an
idle thread. Start with one CPU and a simple round-robin policy. SMP is a later
extension after locking and per-CPU entry state are proved.

Acceptance gate:

- at least two CPU-bound and two blocking threads make timer-accounted progress
  after bounded setup yields;
- register, stack, address-space, domain, and CSpace identity survive at least
  one million forced identity transactions;
- blocked threads consume no runnable slots and wake exactly once; and
- process exit during preemption leaves no runnable use-after-free state.

The scheduler now uses exact 22-word normalized register frames, FIFO runnable
and blocked queues, a staged prepare/install/commit transaction, timer
preemption, synchronous yield, sleep deadlines, wake reasons, bounded priority
quanta, accounting, and a ring-0 idle path. Two CPU-bound and two sleeping CPL3
threads run across two processes and two domains. The stress gate performs one
million complete forced identity transactions. Every iteration verifies the
saved register canary and guarded stack identities together with CR3, TSS.RSP0,
GS entry stack, PCB, domain, address space, and CSpace. The IRQ and SYSCALL
paths separately prove real save/restore and IRETQ switching. Timer-selected
CPL3 frames are return-validated and have RFLAGS sanitized. The syscall-switch
path applies the same rule to a different selected TCB; a CPL3 thread sets NT,
is preempted, and is later selected by another thread's scheduling syscall
before proving that the hostile flag was cleared. A separate CPL3 probe presents
an unmapped saved RSP at yield; the scheduler retires only that TCB and continues
with a sibling. Failed identity installation restores and verifies the
management CR3/TSS/GS state and deschedules any published PCB before returning
the prepared TCB to the FIFO. The million-iteration transaction stress does not
enter CPL3; the IRQ/SYSCALL phase is the real frame-save and IRETQ evidence.

`ocore/kernel/smoke-scheduler-qemu.sh` is the executable acceptance gate. It
requires the million-transaction proof, all four CPL3 progress markers, cooperative
yield, cross-thread hostile-RFLAGS sanitization, exactly-once timer wakes,
preemptive process exit, sibling progress, stale TCB rejection, frame
reclamation, and a post-lifecycle timer.

The scheduler remains a bounded single-CPU proof. It has four TCB slots, no
SMP locking, no FPU/SIMD context, no load balancing, and no production fairness
or denial-of-service claim.

### Milestone 3: IPC, shared memory, and capability transfer

Status: **bounded native IPC gate implemented**.

Probe mode 13 implements and gates the first dependency slice: eight
generation-tagged endpoint objects with bounded FIFO queues and deterministic
cancellation cleanup; invisible generation-tagged destination-slot
reservations; generation-tagged memory-transfer tickets; attenuation-only
cross-CSpace copies into a kernel-selected slot; exact-generation escrow for
queued destination capabilities; and one optional RW/NX shared page mapped into
each of two independent address spaces through its exact owner CSpace. The QEMU
proof writes the page through one CR3, reads the nonce through the other,
requires exact attenuation and denies re-transfer, aborts and rejects a stale
transfer ticket, proves destination capability generation reuse, exercises
endpoint backpressure/FIFO/correlation cancellation and waiter-record cleanup,
rejects new work from a dead sender, explicitly cancels that sender's prior
queue item and ticket from the management harness, and reclaims every resource.
These waiter records exercise bounded registry
bookkeeping, not live blocked TCBs.

The mode-13 foundation remains as a narrow regression gate. Mode 14 builds on
it with public generation-safe endpoint create/send/receive/cancel syscalls,
real TCB block/wake epochs for empty receive and full send, exact attenuation
during atomic cross-CSpace transfer, and lifecycle-driven queue cleanup. The
endpoint capability authorizes derivation of its receiver CSpace; the transfer
ticket then binds the exact creating process generation and that destination
CSpace, not the endpoint object. Mode 14 fills all 16 bounded ticket slots,
denies abort when a different process is handed the raw ticket, lets the owner
abort all 16 exactly once, rejects a repeated stale abort, and creates a fresh
ticket to prove exhaustion recovery.

`ocore/kernel/smoke-ipc-qemu.sh` boots four CPL3 processes: a client, healthy
personality service, crashing personality, and unrelated observer. It requires
cross-domain request/reply, five ordered replies through a bounded FIFO, one
real full-queue block and wake-once retry, automatic dead-sender cleanup,
contained exception-driven service failure, continued unrelated-world
progress, transactional teardown, total resource reclamation, a later timer,
and one second of survival. The script also rejects foundation-only wording and
Linux markers, so its transcript cannot be mistaken for either the older gate
or a foreign-ABI result.

This closes the milestone at that fixed one-CPU scenario. It does not establish
SMP IPC, unbounded queues, a complete interleaving matrix for every possible
sender/receiver death state, or the request-scoped foreign-memory protocol in
[`PERSONALITY_MEMORY_VIEW.md`](PERSONALITY_MEMORY_VIEW.md). `cap_copy` now
uses an authorized endpoint to select its receiver CSpace and prepares an
attenuation-only ticket bound to the exact creating process generation and
that CSpace. It is not endpoint-object-bound, and it is not an ambient
operation that lets a sender choose a destination CSpace slot.

### Milestone 4: native loader, VFS objects, and service namespace

Status: **bounded native loader/VFS gate implemented**.

The implementation validates static x86-64 `ET_EXEC` files, at most eight
`PT_LOAD` segments, file/memory extents, entry placement, overlap, and W+X; it
rejects `PT_INTERP` and `PT_DYNAMIC`. It materializes file bytes and zero BSS,
uses exact RX, R/NX, and RW/NX PTEs, and constructs an aligned minimal SysV
stack in the fixed `0x02000000..0x02100000` loaded-image window. The immutable
OVFS backend, domain-relative mount/process namespace, and capability-returning
service registry are generation-safe and fixed-capacity.

`ocore/kernel/build-m4-artifacts.sh` builds two separately linked static O-core
personality ELFs plus malformed, overlapping, and W+X corpus files, then
requires deterministic `OVFSIMG1` repacking and a host-verified SHA-256.
`ocore/kernel/smoke-loader-qemu.sh` proves the payloads are image data rather
than kernel-linked symbols, rejects the corpus before start, executes both
personality ELFs in separate address spaces at the same preferred virtual
window, checks BSS and loaded W^X, resolves a service to an attenuated
consumer-CSpace capability, transactionally tears down services, mounts, and
processes, reclaims all frames, and reaches a later timer.

The native importer recomputes SHA-256 over the complete embedded artifact
before validating OVFS structure and publishing the recorded identity. This
milestone has no dynamic linker, shared-library ABI, writable filesystem,
demand paging, general path API, or foreign executable format.

### Milestone 5: native live system and package activation

Status: **bounded native live-system gate implemented**.

Mode 16 loads native `init`, supervisor, package-daemon, and serial-REPL ELF
files through the Milestone 4 loader. Each has a separate address space and
CSpace. A fixed-capacity immutable package-root registry, exact default-deny
capability requests, health-gated service generations, and transactional
activation sit behind a typed control capability held only by the REPL.

The hosted semantic oracle in
[`HOSTED_LIVE_REFERENCE.md`](HOSTED_LIVE_REFERENCE.md) remains broader: it
executes failed upgrades, rollback, targeted restart, reconstruction, and
cross-world OValue composition with host child processes. Those hosted results
do not substitute for, and are not implied by, the native gate.

The current compiler-bootstrap stage is hosted: a pinned local `ocorec` builds
packages and injects an immutable image whose payload digest is checked before
boot. Source and compiler digests belong in the fuller build receipt but are not
claimed by this gate. A later capability-bounded builder endpoint and eventual
compiler domain use the same build-request contract. The first usable live
system does not wait for native self-hosting.

`ocore/kernel/build-m5-artifacts.sh` performs two builds of all four static
service ELFs and the read-only OVFS image, requiring byte identity and the exact
host-computed image digest. The pinned artifact is 62,056 bytes with SHA-256
`388b9253ce6f92bef1e1f986b46aabbeb728604cc73589d12105031f5f6b780a`;
the kernel recomputes that digest before OVFS import. `smoke-live-qemu.sh`
verifies the ELF entry symbols are absent from the kernel, boots the four loaded
CPL3 principals, and drives the real serial `o> ` loop. It submits malformed
install text, then an exact-digest install, then exact-digest activation. The
malformed command must publish no state. Installation publishes one immutable
package root; activation grants only the five declared rights requests,
health-gates all four service-generation records, and publishes the complete
set atomically.

After activation, the package-daemon ELF deliberately faults in CPL3. Mode 16
contains only that process generation while all three unrelated services reach
their own completion publications. It reaps the old process, thread, CSpace,
address space, and debug capability; withdraws the old service while the native
control state is `CONTROL_RECOVERING`; rejects every stale generation; and loads
a replacement package daemon from the same verified image. The replacement is
not republished until its exact private restart-health token is observed. The
gate then reaches `CONTROL_DEACTIVATED`, revokes the REPL control capability,
tears down namespace and process generations, reclaims every dynamic frame,
reaches a post-lifecycle timer, and survives the following observation window.

The separate mode-17 `smoke-live-semantics-qemu.sh` gate proves the broader
finite state corpus: two immutable roots, overgrant/incomplete-set denial,
failed-health nonpublication, complete-set rollback, stale references, abstract
crash/restart with unaffected state, strict parser behavior, invariants, and a
post-test timer.

This proves the first host-assisted native live substrate, not the full target
architecture in [`LIVE_SYSTEM.md`](LIVE_SYSTEM.md). The three non-REPL service
ELFs are isolated startup/completion principals; package and supervisor state
machines remain privileged behind the control syscall rather than cooperating
user-space daemons over endpoint RPC. The native process-level restart proof is
exactly one package-daemon generation, not general or unbounded retry/backoff.
A replacement that faults or omits the exact health token remains withdrawn and
fails closed; a further restart is not proved. The gates also do not yet prove
two-package dependency resolution, failed-upgrade rollback through real serial
input, durable reboot reconstruction, compiler receipts, native self-hosting, a
dynamic linker, framebuffer support, a Linux ABI, a foreign root filesystem, or
arbitrary hosted O backends inside O-core.

### Milestone 6A: package-loaded scalar personality supervision

Status: **bounded scalar gate implemented**.

Mode 18 packages four independently linked static ELFs at
`/sbin/m6-client.elf`, `/sbin/m6-personalityd.elf`,
`/sbin/m6-supervisord.elf`, and `/sbin/m6-observer.elf`. The deterministic
62,104-byte OVFS image has SHA-256
`f5924eeb64b5a3d332e20b5d0fae7b233ae2714eb58b72ea07f08a4d26334417`;
the host verifies the exact path set, byte identity, and digest, and the kernel
recomputes the digest before import. The gate rejects any kernel-linked user
module symbol.

The client runs under a minimal test-personality ID. Its generation-bound call
capability enters the common syscall router, allocates a bounded request record,
and forwards one packed operation/scalar pair through an endpoint to the
unprivileged native personality daemon. The daemon's typed reply capability is
generation-specific. Reply, unprivileged-supervisor cancellation, deadline
expiry, and service death compete for exactly one terminal transition and one
dependent-thread wake; late and duplicate completion cannot change the result.
Consumed terminals enter a 16-record exact-handle history. The gate requires
all nine records and zero eviction; replies older than the bounded history stay
denied and are conservatively classified stale.

The independently loaded CPL3 supervisor performs the policy sequence: direct
health RPC, publish generation 1, cancel a held request, observe the
service-owned endpoint close after the daemon's deliberate fault, request a
fresh generation, health-gate generation-2 publication, and request cooperative
stop. It queues the fault watch before cancellation releases the client, using
the shared endpoint FIFO as the watch-before-timeout/crash barrier. O-core
validates authority and performs routing, containment,
load/reap/rebind, and terminal arbitration as mechanism; it does not choose that
policy. The executable corpus proves ping, add-one, explicit unsupported,
cancellation, timeout, service-death failure, stale and duplicate replies,
denial of the stale generation-1 call capability, unrelated-observer progress,
complete reclamation, and a later timer.

This is deliberately M6A rather than full Milestone 6. The test-personality
surface is scalar-only. Pointer-bearing calls and direct endpoint access are
disabled, so there is no shared or request-scoped foreign memory view. The gate
also does not establish general package dependency resolution, durable reboot
reconstruction, unbounded retry/backoff, a foreign executable format, or a
Linux or other foreign operating-system ABI.

### Milestone 6B: request-scoped memory and delegated-resource completion

Status: **bounded native mechanism plus live four-byte vertical slice
implemented; complete M6B remains in progress**.

Mode 19 implements the first bounded-copy portion of
[`PERSONALITY_MEMORY_VIEW.md`](PERSONALITY_MEMORY_VIEW.md). Four
generation-tagged request views use kernel-owned staging, direction-attenuated
nontransferable capabilities, a hard 128-byte per-view/256-byte aggregate
budget, immutable input snapshot, and written-prefix-only output commit after
exact process/address-space revalidation. Reply, cancellation, timeout,
service-death, process-exit, unmap, and resource-revocation hooks close the view
capability before one terminal result and one wake publication. If process-exit
or unmap makes an already replied view undeliverable, the hook releases its
staging and quota without a second result or wake. Stale and duplicate use is
denied.

The same gate adds independently revocable typed lease objects for memory,
filesystem, timer, network, and device classes. Each lease carries a nonzero
request identity, may bind only to a view for that exact request, and may be
revoked with every same-request lease and bound live view while unrelated
requests survive. It proves no ambient fallback. These lease kinds are
authority skeletons, not
implementations of a filesystem, network stack, timer service, or device
driver. The mode-19 scenario exercises a real native process/address space but
invokes process-exit/unmap and wake-publication hooks directly; it is not yet
integrated with real process teardown, mapping mutation, scheduler wake, the
M6A CPL3 daemon, or a public pointer-bearing personality call.

Mode 24 supplies one deliberately narrow live integration. Four independently
linked, digest-pinned CPL3 ELFs run as client, personality daemon, supervisor,
and unrelated observer. One exact four-byte `INOUT` shape crosses the public
bounded-call syscall, M6A router, request-correlated view lookup/read/write,
and bounded reply without client reissue. After one contained generation-1
daemon fault, the supervisor health-gates generation 2 and selects
pre-terminal unmap, request-revoke, delegated-device-resource-revoke, and
caller-exit dispositions while the matching requests are still waiting. These
policy-triggered dispositions do not mutate a mapping or observe an external
resource event. The delegated device resource is one internal typed lease, not
a physical device. The gate does not exercise the post-reply/pre-consume
process-exit or unmap race and establishes no Linux or Plan 9 boot, general
foreign ABI, general guest agent, PCI/DMA/IOMMU, or physical-device boundary.

Complete M6B still requires pinned-window and streaming semantics, actual
unmap/protection/signal and external-resource integration, a pinned Linux
oracle, broader request shapes and service adapters, schema fuzzing,
allocation-failure injection, and the remaining concurrent teardown races. It
does not itself establish Linux ABI compatibility.

### Milestone 7: minimal translated Linux x86-64 personality

Status: **first exact bounded slice implemented; complete Milestone 7 remains
in progress**.

Mode 25 packages one pinned 8,520-byte static, single-threaded Linux x86-64
ELF with a native personality daemon, supervisor, and unrelated observer. The
foreign ELF executes at CPL3 and uses exactly two 20-byte `write` calls, one
unsupported syscall returning Linux `-ENOSYS`, and `exit_group(42)`. Linux fd
integers are resolved to generation-bound internal objects only after the
caller and service identities have been validated. A successful stdout
terminal remains consumable across one contained daemon fault. The private
generation-2 replacement first denies stale generation-1 lookup, then answers
health and is published; only afterward does the client consume stdout and
proceed to stderr before complete reclamation.

Mode 26 retains that exact Linux ELF and adds an unprivileged native 9P2000
server, native supervisor, and independently linked native Plan-9-style client.
The four principals load from one deterministic 92,872-byte immutable OVFS
image into isolated CPL3 address spaces. The server exposes only the
generation-bound `/srv/linux/status` path, where the client performs exact
`version`, `attach`, `walk`, `open`, `read`, and `clunk` exchanges with
`msize = 128`. One contained server fault withdraws generation 1 before a
health-gated generation-2 replacement; stale generation-1 call authority is
denied, both clients survive, resources are reclaimed, and a later timer fires.

Mode 26 is real bounded 9P2000 wire behavior between native O-core principals.
It is not a Plan 9 kernel or binary, a general 9P server, namespace, mount
environment, network transport, persistent filesystem, or guest-agent path. It
does not boot Linux or Plan 9 and adds no general Linux ABI, hardware
virtualization, PCI/device assignment, DMA/IOMMU isolation, or physical-device
evidence.

Mode 26 also does not prove provider routing or request fallback. Generation 2
is a replacement instance of the same server implementation, and it serves a
later, different 20-byte snapshot only after generation 1's read and clunk have
completed. The gate has no two-provider route set for one immutable object,
requester-local route choice, recovery of one logical read on another provider,
fresh second-provider session/fid reconstruction, causal multi-attempt trace,
or live `OWRECEIPT` emission.

#### M7B: two-provider immutable 9P read fallback

Status: **partially implemented**. Mode 31 passes the bounded M7B-1 local
mechanism gate; the complete milestone, persistent attempt evidence, live
receipt binding, provider-implementation diversity, and later KernelWorld
escalations remain open.

M7B is intentionally narrower than general WorldFS or general retry. Its first
qualifying gate should require all of the following:

- one native client and an unprivileged requester-local route coordinator;
- two independently admitted native CPL3 9P provider principals, both bound
  before the request to the same immutable, content-addressed 20-byte object
  under one logical operation but with distinct provider identities;
- a first attempt on provider A that is accepted and then ends in a valid
  terminal failure or withdrawal before successful logical settlement;
- local exclusion of A and stale denial of A's generation-bound call authority,
  without describing that observation as owner revocation or physical
  reclamation;
- a second attempt on provider B using fresh `version`, `attach`, `walk`,
  `open`, `read`, and `clunk` exchanges and a fresh provider-local fid;
- exactly one successful logical result whose digest matches the admitted
  object, plus a causal attempt trace recording A failure before B success;
  and
- complete bounded reclamation of A and B request/session resources while an
  unrelated service remains live and a later timer fires.

Mode 31 implements that first mechanism boundary in
`ocore/kernel/smoke-m7b-logical-read-qemu.sh`. One deterministic provider ELF is
instantiated in two isolated provider processes with distinct process/resource
generations, CSpaces, address spaces, endpoints, service bindings, and call
capabilities. A requester-local client/router principal has both routes before
the request while B's service loop remains staged. A completes fresh 9P setup
and returns a valid terminal `Rerror`; O-core then removes A's local route,
retires its call authority, and the client proves the retained numeric handle
stale. B starts only afterward and completes a fresh setup/read/clunk with
provider-local fids, exact 20-byte bytes, and the pinned object digest.

The kernel's bounded causal state requires A terminal failure and withdrawal
before B activation, then B read, digest validation, and clunk before cleanup.
It separately reports local route withdrawal, A owner-side physical/process
cleanup, B session/queue cleanup, complete bounded resource reclamation,
unrelated-witness survival, and a later timer. This state is volatile and
non-persisted; its serial transcript is executable gate evidence, not a signed
trace or live `OWRECEIPT`.

M7B-1 intentionally combines requester and route coordinator, and both
provider principals instantiate the same provider artifact. It therefore proves
two coexisting, independently revocable provider principals rather than two
independent implementations or packages. A later diversity claim requires
distinct admitted provider artifacts and contracts. If M7B claims a live
`OWRECEIPT`, it must first connect the currently offline Mode 30 format to the
live request, object, provider generations, attempt outcomes, and settlement.
An offline receipt fixture is not a substitute.

M7B would still not establish mutable writes, fid migration, general 9P or
WorldFS, network transport, exactly-once effects, Governor consensus, G7/G8,
foreign-kernel boot, hardware virtualization, PCI/device assignment,
DMA/IOMMU isolation, or physical-device evidence.

These slices intentionally do not complete the broader milestone. Grow it in
dependency slices: process identity and exit, console I/O, file descriptors and
path lookup, virtual memory, time, signals, then `clone`/`futex` for threads.
Candidate calls include `read`, `write`, `openat`, `close`, `mmap`, `munmap`,
`brk`, `exit`/`exit_group`, `clock_gettime`, `getpid`, `ioctl`,
`rt_sigaction`, `rt_sigprocmask`, `clone`, and `futex`.

The broader Linux service must be activated as an immutable
`personality/linux` package through the general Milestone 5 path, not compiled
into privileged policy. Mode 25 is a deliberate bounded-copy exception for its
exact two input views; before the pointer-bearing Linux surface or executable
corpus expands, the remaining acceptance matrix in
[`PERSONALITY_MEMORY_VIEW.md`](PERSONALITY_MEMORY_VIEW.md) must pass.

Acceptance gate:

- pinned unmodified static x86-64 Linux test binaries run from load to exit;
- syscall argument, errno, structure layout, signal, and memory tests compare
  against a pinned native Linux oracle;
- unsupported syscalls return the documented error and never silently succeed;
- Linux file descriptors and PIDs remain personality objects, not raw O-core
  handles; and
- crashing or stopping the Linux personality cannot stop a native O-Domain.

The completed milestone remains a minimal compatibility slice, not general
Linux binary compatibility and not a Linux kernel. Modes 25 and 26 are narrower
still and do not by themselves satisfy the native-oracle, signal, mapping,
filesystem, or multi-binary acceptance items above.

### Milestone 8: multiple root filesystems and Linux O-Domains

Define a reproducible rootfs image manifest with architecture, content digest,
personality ABI version, mount policy, and required capabilities. Instantiate
at least `linux[alpine]` and `linux[debian]` from separate read-only bases plus
separate writable overlays. Both use the same Linux personality implementation
and receive distinct process, service, mount, and capability namespaces. The
personality and root filesystems are separate package kinds. An illustrative
control flow is:

```text
o> pkg.install("personality/linux")
o> pkg.install("rootfs/alpine")
o> world.create("alpine", personality="linux", rootfs="alpine")
```

These spellings become public API only when the live-system parser and runtime
tests pin them.

Acceptance gate:

- the same pinned static Linux binary runs unmodified in both domains;
- `/`, package metadata, environment, PIDs, writes, and service names remain
  isolated;
- reboot/recreate behavior is deterministic from the manifest and overlay;
- deleting one overlay cannot damage the other rootfs; and
- image provenance and hashes are emitted with the runtime evidence.

### Milestone 9: OValue, capability, and native-capsule crossings

Provide three explicit cross-domain channels:

1. **OValue transport** for bounded structural data such as numbers, text,
   bytes, lists, maps, tables, graphs, and errors;
2. **capability transport** for live resources such as files, shared memory,
   sockets, devices, services, and processes; and
3. **native capsules** for domain-affine objects whose semantics cannot be
   honestly normalized.

The O-Domain coordinator projects these crossings into one typed computational
graph. Structural values, live resources, actor state, effects, and completion
remain distinct nodes. Personality operations are effectful hyperedges, not
untyped RPC strings, and scheduling never treats a capability value as ordinary
serializable data.

Reuse the hosted OValue vocabulary where appropriate, but define a versioned,
bounded kernel transport schema rather than deserializing arbitrary Rust
objects in the kernel. Enforce message size, depth, node-count, and allocation
quotas. Capability values carry opaque transport references; the kernel's
atomic transfer operation supplies authority. Native capsules carry origin
domain, personality, type, lifetime, and rehydration policy, and default to
same-process or never when portability is unproved.

Acceptance gate:

- a native domain and both Linux domains exchange a structural OValue without
  losing the tested type/identity properties;
- one pinned graph routes a value through native and Linux operations, records
  each personality state/effect transition, and fails downstream operations
  deterministically when an upstream domain or capability is revoked;
- a read-only file or shared-memory capability crosses with strictly
  attenuated rights;
- replayed, forged, stale, cross-session, and metadata-escalated references are
  denied;
- a capsule cannot be consumed outside its declared affinity; and
- hostile depth/size/cycle inputs stay within configured CPU and memory bounds.

### Milestone 10: persistent Lisp personality

After the minimal Linux proof establishes compatibility and the three crossing
channels establish composition, implement Lisp as the preferred second foreign
personality. The purpose is not to imitate POSIX. It is to preserve a Lisp
world's native object and control model while making its boundaries explicit.

Acceptance gate:

- `lisp[research]` saves a versioned object image, stops, restores, and
  preserves the pinned object identities and package state;
- live compilation replaces one service generation and existing clients
  observe the documented rebind behavior;
- a pinned conditions-and-restarts corpus retains its control semantics;
- the serial environment can inspect live objects without receiving ambient
  kernel authority;
- the Lisp domain consumes a structural OValue from a Linux domain and calls
  one capability-bounded native O-core service; and
- a Lisp image or service crash leaves the Linux and native domains running.

This milestone is the preferred sequencing decision, not a claim that a Lisp
personality exists now. NT and Darwin remain later ecosystem projects.

### Milestone 11: full-kernel domain mode

Add a subordinate-kernel backend using hardware virtualization when available
and a clearly separated software fallback if one is ever implemented. The VM
boundary owns guest physical memory, vCPUs, interrupt injection, and
paravirtual devices. Direct device passthrough is forbidden in the initial
mode. It may be introduced only after IOMMU-backed DMA isolation, reset,
revocation, and hostile-device tests have their own acceptance evidence.
Crossings use the same OValue/capability/capsule contracts through a guest
agent, never implicit host access.

The execution-neutral Stage-0 contract for this milestone is implemented in
`src/kernel_world.rs` and documented in
[`KERNEL_WORLD_CONTRACT.md`](KERNEL_WORLD_CONTRACT.md). It strictly validates
`ocore.kernel-world/v1` manifests for both `source_integrated` and
`binary_contained` providers, then supplies a bounded host-side lifecycle oracle
for health-gated exports, generation identity, quotas, one-terminal request
disposition, policy-constrained replacement, and provenance. Its integration
test is `tests/kernel_world_contract.rs`. The same gate binds the inner world
declaration to exact name, version, architecture, health, services, capability
requests, and digest from a verified `ocore.package/v1` object. For a
`package_payload` image it also verifies the captured image bytes against the
declared SHA-256. A `user_supplied` image carries an expected-digest constraint;
this stage does not accept or verify those external bytes. Request kinds are
unique. Each device-plane export names its exact existing `device.*` request
through `authority_request`, while non-device exports must omit that field;
export names and protocols never derive authority. Multiple exports may share
one request, and `max_devices` counts distinct bound authority requests. The
reserved rights matrix is `vm.machine` -> `run|stop` and `device.*` ->
`reset|dma`; other kinds cannot borrow those reserved rights.

A bounded native follow-on now encodes that verified object into a deterministic
hash-pinned `OKWORLD1` V2 normal form and parses the actual embedded record in
mode 20. The current fixture record is exactly 459 bytes with SHA-256
`0ece5f7f37ebe203d03cc7e5213dc8f9257a9a225a73e52d37d1f718424b9232`;
its complete backend requirement list is exactly the canonical
`["npt", "svm"]`. V2 retains and validates each exact export-authority key,
unique request kind, typed right, and distinct device-authority charge. Native
supervisor admission keeps package and manifest digests distinct and resolves
each capability request through independently registered, exact-package and
byte-exact kind/purpose policy with default denial; string hashes are never
authority. The same gate
constructs generation-bound VM and vCPU identities and aligned guest-page
attachments backed by anonymous memory objects, checks overlap and quota
denial, and proves exact-world revocation/reclamation while an unrelated VM
survives. Sealing the local pilot graph leaves package admission in `ADMITTED`;
it neither advances provider lifecycle nor claims to fulfill the manifest's
complete machine or memory declaration.

Those objects are deliberately nonexecuting. Mode 20 does not start or
health-check a provider, publish an export, boot a guest, enter VMX/SVM, create
EPT/NPT mappings, execute firmware, inject interrupts, assign a device, map
DMA, or configure an IOMMU. It therefore does not complete any Milestone 11
acceptance item.

Mode 21 adds the first hardware execution slice without changing Mode 20's
claims. Its host gate requires read/write KVM access and the exact CPU features
`svm` and `npt`; the kernel then byte-compares the hash-pinned V2 record's
complete requirement vector to exactly `["npt", "svm"]` before initializing
SVM. On that AMD x86-64 backend it maps two existing guest-page objects through
a private four-level NPT, enters a real-mode synthetic guest, injects a bounded
interrupt, validates a guest computation, handles `VMMCALL`, and receives an
NPF for an unmapped GPA. Stop clears the entire SVM/NPT context and releases
retained page mappings; a second run, generation revocation, unrelated-VM
survival, and a post-execution host timer are asserted by
`smoke-kernel-world-execution-qemu.sh`.

This remains a synthetic-guest VM substrate only. No live device service or
device capability, guest agent, shared queue or shared ring, Linux or Plan 9
boot, virtual device, PCI assignment, IOMMU isolation, DMA mapping, device
reset, or 9P service exists in this slice. It also makes no provider-health or
export-publication claim.

Mode 22 adds a separate TCG-compatible administrative lifecycle slice without
using Mode 21's SVM path. `kernel_world_boot.oc` binds a hash-pinned admitted
world to a configured VM identity and an exact consumer CSpace, requires the
independently granted `vm.machine:run` request for its administrative start,
gates export publication on an exact observed health-protocol ID, and installs
generation-tagged nontransferable client capabilities. Its status operation
reports the native boot generation. Its device-plane reset operation accepts
only O-core broker intent derived from an exact independently granted
`device.*:reset` authority; it does not reset a device or dispatch to a
provider. Duplicate live consumer-CSpace/name/protocol ID tuples are denied.
Generic capability close is registry-aware; retiring the final export returns
the administrative boot to `HEALTHY`, permitting clean republish or teardown.

The Mode 22 failure transition withdraws bindings and closes client
capabilities before revoking the exact VM graph. It retains admission only long
enough for the declared `on_failure` policy to authorize a fresh VM/boot/service
generation, while stale capabilities remain denied and an unrelated instance
survives. Stop/failure tombstones can be removed for uninstall only by one
serialized transition that first proves the active boot and exact local VM graph
are absent, then revokes admission; a configured un-staged replacement makes
uninstall fail unchanged. There is no public abandon operation. Lifecycle and
broker mutations use a single-CPU operation owner and linearization epochs. A
future SMP implementation needs an atomic kernel lock.

This is lifecycle algebra, not a running driver domain: start and health/failure
are invoked directly by the semantics gate, the declared health timeout is not
enforced, and no process, guest, provider image, guest agent, shared transport,
device assignment, DMA/IOMMU boundary, physical reset, 9P service, Linux boot,
or Plan 9 boot is present.

Mode 23 is a bounded composition slice. It uses QEMU TCG to emulate the
AMD SVM/NPT architectural interface, then binds the one available execution
session to an exact generation-tagged boot, admitted world, configured VM,
vCPU, two guest pages, device-plane export, and granted authority request.
O-core performs nested guest entry and receives VMEXIT through that emulated
CPU interface; this is not KVM or physical-hardware isolation. A cross-world
vCPU is denied before SVM/NPT or the virtual endpoint becomes live, and an
execution pin prevents VM-graph destruction while the backend owns retained
page mappings.

The fixed synthetic real-mode guest first performs the exact `VMMCALL` that the
coordinator treats as health. The health protocol identifier comes from the
bound admitted world, not from guest data. After publication, the guest issues
one intercepted 32-bit `OUT` to port `0xE0`. Full IOIO-exit validation precedes
dispatch to one generation-tagged, kernel-internal XOR endpoint. The endpoint
returns `input XOR 0xA5A55A5A`; it is not a physical or QEMU-assigned device.
The client reset-request capability reaches that exact live assignment and
clears its scalar transaction state, without performing hardware reset.

One exact NPF supplies the bounded synchronous failure event. The coordinator
first stops SVM, clears NPT, releases mappings and the execution pin, then
revokes the virtual endpoint, and finally enters boot failure. Boot failure
withdraws the old client capability before exact VM-graph revocation. An
independent published service survives. `on_failure` creates generation-2 VM,
boot, execution-session, endpoint, and client identities; the stale session
and capability are denied while the replacement repeats health and device
execution. Orderly teardown reclaims both worlds and reaches a later timer.

Mode 23 does not boot Linux, Plan 9, firmware, or a supplied user image and
does not implement a general guest agent, shared queue/ring, asynchronous
request transport, or SMP synchronization. It assigns no PCI or physical
device and establishes no DMA window, IOMMU isolation, interrupt remapping,
or hardware reset. Its QEMU-TCG evidence is not an AMD-KVM or hardware
isolation result.

Acceptance gate:

- a pinned Linux kernel and rootfs boot as `linux.kernel[0]`;
- translated and full-kernel Linux domains expose the same versioned external
  service contract where their supported features overlap;
- guest compromise tests cannot access another domain or O-core memory;
- snapshot, stop, restart, resource quota, and paravirtual-device revocation
  behavior are reproducible; and
- no acceptance result depends on direct hardware passthrough.

Only after these gates should additional ecosystem-scale personalities such as
OpenBSD, NT, or Darwin be scheduled. Their native object models remain
personality-specific and must not enlarge the privileged O-core mechanism set
merely for convenience.

## 5. Cross-cutting invariants

These are the target invariants for the architecture. Milestones 0.2 and 0.3
enforce kernel-owned routing, mapping-aware pointer checks, fault-aware bounded
copies, typed capability lookup, user and kernel W^X, guarded stacks, normalized
fault frames, validated return state, generation-safe frame and memory-object
reuse, zero-before-reuse, and per-CSpace allocation quotas. Milestones 1 and 2
make independent address spaces, teardown, and scheduling executable. Milestone
3 makes public bounded endpoint IPC, real TCB block/wake, attenuated transfer,
death cleanup, and crash containment executable. Milestone 4 adds static native
ELF/OVFS/namespace lifecycles, and Milestone 5 composes four loaded principals
with capability-gated serial package activation plus one health-gated
package-daemon replacement. M6A adds a package-loaded scalar test personality,
an unprivileged endpoint-backed daemon and supervisor, terminal request
arbitration, and one generation rebind. M6B's first slice adds bounded-copy
request views and typed delegated-resource revocation as a separate native
mechanism gate; Mode 24 composes that mechanism with one exact four-byte live
CPL3 call shape, a contained daemon fault, one generation rebind, and bounded
pre-terminal lifecycle dispositions. Mode 25 adds one exact static Linux ELF,
two bounded input writes, Linux `-ENOSYS`, direct exit status 42, preserved
terminal consumption across daemon replacement, and stale-generation denial.
Mode 26 composes that same Linux corpus with one exact native 9P2000 server and
Plan-9-style client path, namespace withdrawal, health-gated replacement, and
stale call-capability denial.
The first KernelWorld native slice adds
hash-pinned normal-form
admission and nonexecuting VM/vCPU/guest-page identities; Mode 21 adds a
hardware-only synthetic SVM/NPT execution substrate; and Mode 22 separately
adds bounded administrative health-gated publication, withdrawal before
VM-graph revocation, policy-constrained replacement, and stale
client-capability denial. Mode 23 composes those boundaries under
QEMU-TCG-emulated SVM/NPT with exact boot/world/VM/vCPU execution ownership,
VMEXIT-derived health and NPF failure, one kernel-internal virtual PIO
endpoint, reset dispatch, ordered quiesce, and generation-2 rebind.
Every statement remains scoped to its fixed-capacity, single-CPU gate.

1. Domain, personality, rootfs, process, thread, and CSpace identities are
   distinct types and cannot be substituted by integer coincidence.
2. Kernel pointers are never returned or interpreted as user authority. A
   negative test may deliberately construct a kernel-range address, but the
   kernel must reject it before dereference.
3. Every authority lookup checks slot bounds, occupancy, generation, object
   type, and requested rights. Delegation can only preserve or reduce rights.
4. All user pointer checks are overflow-safe and cover the full byte range.
   Milestone 0.2 checks a concrete immutable bootstrap region and copies through
   exact page-fault fixups. Modes 19 and 24 test explicit unmap dispositions;
   Mode 24's supervisor invokes its disposition before terminal reply and does
   not mutate the mapping. Mode 25 snapshots two exact 20-byte `IN` views and
   likewise does not test concurrent mapping mutation. Concurrent mapping-change
   integration and pinning remain future work.
5. Executable mappings are not writable. Anonymous stacks and data are NX.
   Milestone 0.2 proves this for the bootstrap kernel, user image, and stack.
6. Syscall and exception return validates user RIP/RSP and sanitizes RFLAGS.
7. The current address space, PCB, personality, domain, and CSpace switch as one
   scheduler transaction.
8. Namespace lookup grants no authority. It returns a capability only after
   policy checks.
9. Personality code receives capabilities to mechanisms, not ambient kernel
   access. A personality failure is contained to its dependent domains.
10. OValues are data, capabilities are authority, and native capsules preserve
    affinity. No transport silently converts one category into another.
11. Unsupported ABI behavior fails explicitly. Compatibility claims name the
    binary corpus, architecture, syscall slice, and execution mode tested.
12. Persistence records version, provenance, and capability rebind policy.
    Live handles are never restored as authority from serialized bits alone.

## 6. Primary risks and controls

| Risk | Engineering control |
|---|---|
| x86-64 entry/return bugs compromise ring separation | Mechanism-only assembly, macro-generated trap stubs, trap-frame tests, negative CPL3 faults, sanitized return state, per-CPU entry data before SMP |
| Linux ABI scope expands without a credible result | Pinned static corpus, syscall-by-syscall conformance oracle, explicit unsupported list |
| Personality code bloats the trusted kernel | Capability-bounded user-space services after the first proof |
| Capability confused-deputy or rights amplification | Typed objects, attenuation-only transfer, call provenance, no authority from metadata or names |
| User-pointer races and fault recursion | Request-scoped memory views, fault-aware copy/pin APIs, bounded commits, no raw personality dereference |
| Scheduler and teardown use-after-free | Explicit state machines, reference ownership, cancellation points, forced-switch stress |
| Rootfs names are mistaken for compatibility proof | Content-addressed image manifests plus execution of pinned unmodified binaries |
| Package metadata is mistaken for authority | Policy-resolved capability requests, immutable digests, transactional activation, no authority from names or manifests |
| Native capsules become an untyped escape hatch | Origin affinity, explicit codec/safety/lifetime policy, conservative `never` rehydration |
| Full-kernel mode bypasses the common security model | Guest-agent crossings, paravirtual devices first, quotas, no ambient host mounts or sockets, no passthrough before IOMMU evidence |
| Demonstrations overstate implementation | Every claim tied to an executable gate, serial trace, corpus hash, and documented non-claims |

## 7. Concise feature matrix

| Feature | Pre-0.1 baseline | Current verified boundary | First multi-rootfs demo | Long-term |
|---|---|---|---|---|
| CPU/privilege | x86-64 ring-0 boot | one CPU, canonical CPL3 frames, timer/SYSCALL switching | isolated x86-64 processes | SMP and hardened entry |
| Domains | none | bounded generation-tagged native worlds; four-process IPC and live-system scenarios | native, Alpine, Debian instances | persistent lifecycle and quotas |
| Personalities | none | native dispatch, package-loaded scalar M6A, exact four-byte Mode 24 test personality, Mode 25's pinned minimal Linux corpus, and Mode 26's native Plan-9-style client/server composition | broader translated Linux x86-64 across multiple roots | persistent Lisp plus user-space and full-kernel backends |
| Address spaces | one identity map | independent CR3s, guarded loaded stacks, exact W^X ELF mappings, optional shared RW/NX page | per-process page tables | demand paging and general shared mappings |
| Processes | none | up to four loaded CPL3 principals in the current gates with teardown and stale denial | multiple isolated processes | complete process/thread lifecycle |
| CSpaces | one small global table | exact owners, endpoint transfer attenuation, isolated service CSpaces, typed REPL control cap | one CSpace per process | general atomic cross-domain attenuation |
| Memory | bump-only frames | typed reclaim, private/shared mappings, bounded-copy request staging, and nonexecuting guest-page objects | mapped per-process objects | discovered RAM, paging, NUMA policy |
| Scheduling | timer marker only | bounded preemptive/blocking TCB scheduler with yield, sleep, IPC wake, and loaded-program completion | preemptive blocking scheduler | multicore policy and accounting |
| IPC | none | public bounded CPL3 endpoints, scalar M6A RPC, mode-19 request views, one exact four-byte Mode 24 composition, two Mode 25 Linux input views, and Mode 26's exact 9P2000 corpus | broader personality RPC plus foreign memory views | supervised personality RPC |
| Loading | kernel-linked code | static native ELF plus one exact static Linux ELF from deterministic read-only OVFS; BSS, SysV stack, W^X, rejection corpus | broader native and static Linux ELF corpora | dynamic loaders per personality |
| Filesystems | none | fixed-capacity immutable OVFS images, domain-relative root mounts, and one exact generation-bound `/srv/linux/status` 9P path; no general filesystem | separate Alpine/Debian roots | versioned overlays and services |
| Live system | none | M5 package activation plus bounded M6A scalar, Mode 24 native, Mode 25 Linux, and Mode 26 9P service supervision/rebind slices | package store, general supervision, reconstruction, richer user-space services | native builds and richer interactive environments |
| Crossings | hosted OValue and broker only | bounded scalar IPC/capability transfer plus request views, one exact four-byte native path, two exact Linux input views, and one bounded Linux-result-to-9P path; no native OValue codec or general foreign ABI | OValue, capability, capsule channels | common contract across all modes |
| Compatibility | no foreign OS ABI | one pinned static Linux ELF with write, -ENOSYS, and exit_group only, reused by one exact 9P2000 composition | the same broader pinned corpus across multiple Linux roots | additional personalities by evidence |

The first credible multi-domain O-Domain demonstration is therefore not either
single-process bootstrap gate. It is the later evidence bundle that boots
native, Alpine, and Debian domains together, runs a pinned unmodified static
Linux binary in both Linux roots, exchanges one structural OValue, transfers
one attenuated file or shared-memory capability, and contains a
personality-service failure without stopping the native domain.
