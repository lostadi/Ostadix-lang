# O-Domain Engineering Plan

Status: draft roadmap. Milestone 0.1 is **complete** by its QEMU runtime
acceptance gate.

This document turns the poly-personality kernel brief into a dependency-ordered
implementation plan for this repository. It is a claim boundary as well as a
roadmap. Items described as planned are not implemented merely because their
types, syscall numbers, setup scripts, or names already exist in source.

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
required outer boundary by resolving a random session bearer to a live kernel
handle before transport, but it is not yet connected to the booted kernel.

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

Implement complete exception gates for faults originating at CPL3, a
kernel-owned trap frame, full register preservation, bounded `copy_from_user`
and `copy_to_user`, overflow-safe range checks, guard pages, syscall
interruption rules, per-CPU entry state, and page-granular kernel RX/R/RW-NX
permissions. Remove global single-entry scratch before enabling nested
interrupts or SMP.

Acceptance gate:

- deliberate user page fault, divide error, invalid opcode, bad stack, invalid
  RIP, and oversized/wrapping buffer terminate only the current process;
- kernel text/data remain inaccessible from CPL3;
- kernel text is RX, kernel read-only data is R/NX, writable kernel state is
  RW/NX;
- faults during user copy return a defined error without corrupting kernel
  state; and
- entry/return tests pass under repeated timer interrupts.

### Milestone 0.3: reclaiming physical memory and memory objects

Replace the bump-only allocator with tracked frames, reference counts, typed
memory objects, deterministic zeroing, allocation quotas, and reclaim paths.
Separate kernel, page-table, executable, anonymous, shared, and device memory.

Acceptance gate:

- allocation/exhaustion/reclaim stress returns every non-pinned frame;
- a reallocated frame contains no prior-process data;
- double free and stale memory handles fail closed; and
- allocator invariants survive injected allocation failures.

### Milestone 1: process model, address spaces, and CSpaces

Give each process its own top-level page table, VM map, user stacks with guard
pages, kernel entry stack, thread set, state machine, and CSpace. Make domain and
process handles generation-tagged. Map kernel pages supervisor-only and apply
W^X and NX at page granularity. Define create, start, stop, exit, reap, and
domain teardown transactions.

Acceptance gate:

- two native processes use the same virtual address for different physical
  pages and cannot observe or modify each other;
- switching CR3 and the current PCB also switches the active CSpace;
- stale process, domain, mapping, and capability handles are rejected; and
- killing one process reclaims its private mappings, stacks, and capability
  references without harming its sibling.

### Milestone 2: threads and scheduler

Add saved thread contexts, runnable and blocked queues, timer-driven preemption,
cooperative yield, sleep/timers, wakeup reasons, priorities, accounting, and an
idle thread. Start with one CPU and a simple round-robin policy. SMP is a later
extension after locking and per-CPU entry state are proved.

Acceptance gate:

- at least two CPU-bound and two blocking threads make progress without manual
  yields;
- register, stack, address-space, domain, and CSpace identity survive at least
  one million forced context switches;
- blocked threads consume no runnable slots and wake exactly once; and
- process exit during preemption leaves no runnable use-after-free state.

### Milestone 3: IPC, shared memory, and capability transfer

Introduce endpoint objects, bounded message queues, request/reply correlation,
blocking send/receive, cancellation, and shared-memory objects. Capability
transfer must be an atomic kernel operation that attenuates rights and creates
a new destination slot. A sender never writes a destination slot number.

Acceptance gate:

- same-domain and cross-domain ping-pong work under preemption;
- queue limits produce defined backpressure;
- transfer cannot amplify rights, revive a stale generation, or smuggle a
  kernel pointer;
- sender death and receiver death have defined cleanup behavior; and
- a personality-service crash affects only processes bound to that service.

### Milestone 4: native loader, VFS objects, and service namespace

Implement a kernel-validated x86-64 ELF loader for native O-core executables,
including `PT_LOAD` validation, page permissions, BSS zeroing, stack/argument
construction, and rejection of overlapping or malformed segments. Add the
minimal VFS object model, initramfs or read-only image backend, domain-relative
root/mount namespace, and capability-addressed service registry.

Acceptance gate:

- two separately built native ELF files load from an image rather than being
  linked into the kernel;
- malformed ELF and permission-overlap corpus tests fail before process start;
- no page is writable and executable after load;
- service lookup returns a capability, not an ambient global pointer; and
- namespace teardown releases mounts, services, and processes transactionally.

### Milestone 5: minimal translated Linux x86-64 personality

Add a versioned Linux x86-64 personality service over the mechanisms above.
Begin with static, single-threaded ELF64 programs. Implement only the syscall
surface required by a pinned test corpus, returning Linux-compatible errors for
unsupported calls. Grow in dependency slices: process identity and exit,
console I/O, file descriptors and path lookup, virtual memory, time, signals,
then `clone`/`futex` for threads. Candidate calls include `read`, `write`,
`openat`, `close`, `mmap`, `munmap`, `brk`, `exit`/`exit_group`,
`clock_gettime`, `getpid`, `ioctl`, `rt_sigaction`, `rt_sigprocmask`, `clone`,
and `futex`.

Acceptance gate:

- pinned unmodified static x86-64 Linux test binaries run from load to exit;
- syscall argument, errno, structure layout, signal, and memory tests compare
  against a pinned native Linux oracle;
- unsupported syscalls return the documented error and never silently succeed;
- Linux file descriptors and PIDs remain personality objects, not raw O-core
  handles; and
- crashing or stopping the Linux personality cannot stop a native O-Domain.

This milestone is a minimal compatibility slice, not general Linux binary
compatibility and not a Linux kernel.

### Milestone 6: multiple root filesystems and Linux O-Domains

Define a reproducible rootfs image manifest with architecture, content digest,
personality ABI version, mount policy, and required capabilities. Instantiate
at least `linux[alpine]` and `linux[debian]` from separate read-only bases plus
separate writable overlays. Both use the same Linux personality implementation
and receive distinct process, service, mount, and capability namespaces.

Acceptance gate:

- the same pinned static Linux binary runs unmodified in both domains;
- `/`, package metadata, environment, PIDs, writes, and service names remain
  isolated;
- reboot/recreate behavior is deterministic from the manifest and overlay;
- deleting one overlay cannot damage the other rootfs; and
- image provenance and hashes are emitted with the runtime evidence.

### Milestone 7: OValue, capability, and native-capsule crossings

Provide three explicit cross-domain channels:

1. **OValue transport** for bounded structural data such as numbers, text,
   bytes, lists, maps, tables, graphs, and errors;
2. **capability transport** for live resources such as files, shared memory,
   sockets, devices, services, and processes; and
3. **native capsules** for domain-affine objects whose semantics cannot be
   honestly normalized.

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
- a read-only file or shared-memory capability crosses with strictly
  attenuated rights;
- replayed, forged, stale, cross-session, and metadata-escalated references are
  denied;
- a capsule cannot be consumed outside its declared affinity; and
- hostile depth/size/cycle inputs stay within configured CPU and memory bounds.

### Milestone 8: user-space kernel personality mode

Move translated personality policy out of privileged code behind capability-
bounded IPC. Define a supervisor contract for startup, health, restart, state
recovery, and process failure when a service disappears. Permit componentized
foreign-kernel services only through explicit memory, endpoint, filesystem,
network, timer, and device capabilities.

Acceptance gate:

- the Linux ABI service and one componentized kernel service run unprivileged;
- revoking a delegated device or network capability immediately removes that
  authority;
- service crash/restart does not crash O-core or unrelated domains; and
- in-flight syscall failure and domain recovery are deterministic and logged.

### Milestone 9: full-kernel domain mode

Add a subordinate-kernel backend using hardware virtualization when available
and a clearly separated software fallback if one is ever implemented. The VM
boundary owns guest physical memory, vCPUs, interrupt injection, and emulated or
paravirtual devices. Crossings use the same OValue/capability/capsule contracts
through a guest agent, never implicit host access.

Acceptance gate:

- a pinned Linux kernel and rootfs boot as `linux.kernel[0]`;
- translated and full-kernel Linux domains expose the same versioned external
  service contract where their supported features overlap;
- guest compromise tests cannot access another domain or O-core memory; and
- snapshot, stop, restart, resource quota, and device-revocation behavior are
  reproducible.

Only after these gates should additional personalities such as OpenBSD, Lisp,
NT, or Darwin be scheduled. Their native object models remain personality-
specific and must not enlarge the privileged O-core mechanism set merely for
convenience.

## 5. Cross-cutting invariants

These are the target invariants for the architecture. Milestone 0.1 enforces
kernel-owned routing, pointer bounds, typed capability lookup, user-image W^X,
user-stack NX, and validated return state within its fixed single-process
layout. Its coarse 2 MiB supervisor mappings still make the kernel region RWX;
U/S blocks CPL3 access, but full kernel W^X remains a hardening requirement.
Later milestone gates make the remaining invariants executable rather than
aspirational.

1. Domain, personality, rootfs, process, thread, and CSpace identities are
   distinct types and cannot be substituted by integer coincidence.
2. Kernel pointers are never returned or interpreted as user authority. A
   negative test may deliberately construct a kernel-range address, but the
   kernel must reject it before dereference.
3. Every authority lookup checks slot bounds, occupancy, generation, object
   type, and requested rights. Delegation can only preserve or reduce rights.
4. All user pointer checks are overflow-safe and cover the full byte range.
   Milestone 0.1 bounds a fixed immutable mapping before dereference;
   Milestone 0.2 replaces that bootstrap rule with fault-aware copy/pin APIs.
5. Executable mappings are not writable. Anonymous stacks and data are NX.
   Milestone 0.1 proves this only for its user image and stack; later page-table
   work must split kernel text, read-only data, and writable NX state as well.
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
| x86-64 entry/return bugs compromise ring separation | Small assembly surface, trap-frame tests, negative CPL3 faults, sanitized return state, per-CPU entry data before SMP |
| Linux ABI scope expands without a credible result | Pinned static corpus, syscall-by-syscall conformance oracle, explicit unsupported list |
| Personality code bloats the trusted kernel | Capability-bounded user-space services after the first proof |
| Capability confused-deputy or rights amplification | Typed objects, attenuation-only transfer, call provenance, no authority from metadata or names |
| User-pointer races and fault recursion | Fault-aware copy/pin APIs, bounded copies, no raw personality dereference |
| Scheduler and teardown use-after-free | Explicit state machines, reference ownership, cancellation points, forced-switch stress |
| Rootfs names are mistaken for compatibility proof | Content-addressed image manifests plus execution of pinned unmodified binaries |
| Native capsules become an untyped escape hatch | Origin affinity, explicit codec/safety/lifetime policy, conservative `never` rehydration |
| Full-kernel mode bypasses the common security model | Guest-agent crossings, explicit virtual devices, quotas, no ambient host mounts or sockets |
| Demonstrations overstate implementation | Every claim tied to an executable gate, serial trace, corpus hash, and documented non-claims |

## 7. Concise feature matrix

| Feature | Pre-0.1 baseline | Milestone 0.1 | First multi-rootfs demo | Long-term |
|---|---|---|---|---|
| CPU/privilege | x86-64 ring-0 boot | one CPL3 native payload | isolated x86-64 processes | SMP and hardened entry |
| Domains | none | one fixed `native[0]` registry entry | native, Alpine, Debian instances | persistent lifecycle and quotas |
| Personalities | none | native-only dispatch | minimal translated Linux x86-64 | user-space and full-kernel backends |
| Address spaces | one identity map | fixed user RX and RW/NX ranges | per-process page tables | demand paging and shared mappings |
| Processes | none | one PCB/current process | multiple isolated processes | complete process/thread lifecycle |
| CSpaces | one small global table | one process-local typed CSpace | one CSpace per process | atomic cross-domain attenuation |
| Scheduling | timer marker only | syscall hook only; no scheduler | preemptive blocking scheduler | multicore policy and accounting |
| IPC | none | none | endpoints, shared memory, transfer | supervised personality RPC |
| Loading | kernel-linked code | linked user payload | native and static Linux ELF loaders | dynamic loaders per personality |
| Filesystems | none | none | separate Alpine/Debian roots | versioned overlays and services |
| Crossings | hosted OValue and broker only | none to booted kernel | OValue, capability, capsule channels | common contract across all modes |
| Compatibility | no foreign OS ABI | no foreign OS ABI | pinned minimal Linux corpus | additional personalities by evidence |

The first credible O-Domain demonstration is therefore not the Milestone 0.1
serial output. It is the later evidence bundle that boots native, Alpine, and
Debian domains together, runs a pinned unmodified static Linux binary in both
Linux roots, exchanges one structural OValue, transfers one attenuated file or
shared-memory capability, and contains a personality-service failure without
stopping the native domain.
