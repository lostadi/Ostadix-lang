# Containing and Utilizing Foreign Kernels in O-core

*A design proposal for hosting Linux, Android, XNU/Darwin, and Windows NT as
O-Domain personalities — written against the existing `ocore` runtime and the
`ODOMAIN_PLAN` roadmap.*

Status: proposal / architecture note. It extends the existing plan; it does not
claim anything the milestone gates have not yet proven. Where it names a target
kernel, it names the honest strategy and the real blocker rather than a slogan.

---

## 0. Deconstructing the request

"Allow my Okernel to contain and fully utilize all other kernels within it" is
one sentence hiding three separate questions. Untangling them is most of the
design work, because each admits a different answer and the mistakes come from
conflating them.

**What does "contain a kernel" mean?** It bifurcates, and your own roadmap
already encodes the fork:

- *Contain the ABI.* Reimplement the application-facing contract a foreign
  kernel exposes — its syscall numbers, errno space, struct layouts, signal and
  object model — and run foreign **binaries** directly on O-core. No foreign
  kernel code executes. This is the **personality / library-OS** path
  (`ODOMAIN_PLAN` M7–M10). Precedents: WSL1, FreeBSD's Linuxulator, illumos
  LX-branded zones, Wine, Darling.
- *Contain the kernel image.* Run the actual Linux/NT/XNU binary kernel as a
  guest, with O-core as hypervisor, and talk to it through a paravirtual
  channel. This is **full-kernel domain mode** (`ODOMAIN_PLAN` M11). Precedents:
  WSL2, gVisor's KVM platform, any Type-1/Type-2 VMM.

These are duals, and the trade is exact: translation maximizes *utilization*
(foreign resources become native O-core capabilities, crossings are cheap,
scheduling is shared) at the cost of *engineering per syscall* and a permanent
compatibility tail. Subordination maximizes *fidelity* (it is the real kernel,
so compatibility is total) at the cost of *integration* (the guest is an opaque
box reachable only through an agent) and of you now maintaining a VMM.

**What does "fully utilize" mean?** This is the load-bearing phrase, and it is
where containment and utilization pull against each other. A VM contains
perfectly and utilizes poorly: the guest's file descriptors, sockets, and
processes are invisible sludge behind a virtio wall. A personality utilizes well
and contains imperfectly: a foreign `fd` can *become* an O-core capability, but
every unimplemented syscall is a leak in the abstraction. "Fully utilize" is not
"emulate everything" — it is *make foreign resources first-class citizens of the
O-Domain graph without granting them ambient authority*. You already have the
membrane for exactly this: the M9 trichotomy of **OValue** (data),
**capability** (authority), and **native capsule** (affinity). That trichotomy
*is* the technical answer to "utilize," and Section 3 argues it is the whole
game.

**What does "all other kernels ... within it" mean?** The word *all* implies
simultaneity — Linux and NT and XNU domains coexisting and *composing* in one
running system. Hosting one foreign kernel is a solved genre; Google, Microsoft,
and Apple each ship a variant. The part that is novel, and that your
architecture is unusually well-positioned for, is the *composition*: a single
typed computational graph in which a value crosses from a Linux process through
an NT process into a native O-core service, each hop typed as data / authority /
capsule, each personality failure contained. That is `ODOMAIN_PLAN` M9, and it
is the actual thesis-grade contribution. Section 5 makes the case.

So the request factors into three orthogonal parameters, and every target kernel
is a point in this space:

| Parameter | Values | Governs |
|---|---|---|
| **P1 — boundary mechanism** | translate · subordinate · hybrid | how the foreign contract is realized |
| **P2 — fidelity** | syscall-exact · errno-exact · layout-exact, over a *named corpus* | what you promise a foreign binary |
| **P3 — crossing semantics** | OValue · capability · native capsule | how foreign resources are *utilized* |

The rest of this document is: a structural framing that says *why* your existing
primitives are the right substrate (Section 1–2), the crossing membrane that
delivers "utilize" (Section 3), a per-kernel map with honest blockers
(Section 4), the composition thesis (Section 5), non-claims (Section 6), and the
concrete next PR (Section 7).

---

## 1. The structural-realist framing: kernels as functors into O

Here is the lens I think actually fits what you are building, and it is not
decoration — it dictates the interfaces.

Strip a kernel to its invariant and every one of Linux, NT, XNU, and Binder is
the **same** structure: a function from `(principal, request, capability-set,
memory-view)` to `(result, effect, next-state)`. What differs between them is
not that structure but its *presentation* — the coordinates in which the
structure is expressed. Linux presents authority as a small-integer `fd`; NT as
a `HANDLE` into the Object Manager; Mach as a `mach_port_t` send/receive right;
Binder as a reference-counted object handle. These are the *phenomena*. The
*noumenon* — the thing they are all presentations of — is a capability-mediated
request over an isolated principal. That noumenon is precisely what `ocore`
already implements: PCB/domain/CSpace as principal, endpoint send/receive as
request, typed generation-tagged capability as authority, and the
`PERSONALITY_MEMORY_VIEW` protocol as the memory-view.

Cast in categorical terms (since parameterization is the point): let each
foreign kernel be a category **K** whose objects are its resource types (`fd`,
`HANDLE`, `mach_port`, `binder_ref`) and whose morphisms are its syscalls. Let
**O** be O-core's category: objects are typed capabilities, morphisms are
endpoint operations under the memory-view protocol. A **personality is a functor
`P: K → O`**. Its job is to preserve the structure that matters — object
identity, the rights lattice (attenuation-only), lifetime/generation, and
failure — while discarding the accidental coordinates.

Three consequences fall out of this framing immediately, and they are why your
existing design choices are the correct ones rather than arbitrary:

1. **Attenuation-only capability transfer** (`cap_transfer.oc`) is exactly the
   requirement that `P` be a functor into the *sub-object/rights lattice* — a
   personality may narrow authority but never forge or amplify it. Your
   `RIGHT_*` model and generation tags are the codomain's object structure.
2. **The memory-view protocol** is the statement that `P` may not smuggle a raw
   pointer across the functor — a foreign address is meaningless in O; only a
   bounded, request-scoped view is a legal codomain object. Your
   `PERSONALITY_MEMORY_VIEW.md` is, in this reading, the naturality condition on
   memory arguments.
3. Because all `P_i` **share the codomain O**, morphisms *between* personalities
   exist for free-ish — they factor through O. That shared codomain is what
   makes "utilize *all* kernels at once" a coherent goal rather than N disjoint
   emulators. (Section 5.)

"Compatibility," then, has a precise definition: `P` preserves the observable
equalities of a *named binary corpus* against a *native oracle*. That is already
the discipline in your M7 acceptance gate — you just have a cleaner name for it.

---

## 2. What already exists to build on

This proposal is cheap because most of the substrate is built and gated. The
foreign-personality work is *additive* over the following, all of which are
present in `ocore/runtime/x86_64/` and gated in `ocore/kernel/`:

- **Principal isolation & mechanism** (M0.1–M1): independent CR3s, W^X, guarded
  stacks, normalized trap frames, typed capability lookup, per-CSpace quotas.
  Foreign processes are just O-core processes with a foreign personality tag.
- **Scheduling & blocking** (M2): preemptive/blocking TCB scheduler with sleep,
  yield, and IPC wake epochs — the substrate a `futex` or `mach_msg` receive
  blocks on.
- **Endpoint IPC + attenuated transfer + crash containment** (M3): public
  bounded CPL3 endpoints, real block/wake, death cleanup. This is the transport
  a personality service speaks.
- **Static ELF loader + immutable OVFS + service namespace** (M4): loads foreign
  static ELF images from a content-addressed read-only image into isolated
  W^X address spaces. A foreign binary is loaded here.
- **Package activation / live supervision** (M5): a personality ships as an
  *immutable package*, activated through the supervisor, health-gated, with
  generation rebind and rollback — never compiled into privileged policy.
- **Scalar personality supervision** (M6A): `personality.oc` already carries
  `PERSONALITY_NATIVE` and a deliberately tiny `PERSONALITY_TEST`;
  `personality_rpc.oc` already routes a versioned request corpus with
  cancellation, timeout, and service-death results and rejects late / duplicate
  / prior-generation / stale-capability use while an unrelated observer keeps
  running. This is the personality **router**, minus real ABIs.

The ABI seam is already reserved in `native_abi.oc`:

```
SYS_PERSONALITY_CALL       = 14
SYS_PERSONALITY_REPLY      = 15
SYS_PERSONALITY_SUPERVISE  = 16
PERSONALITY_RPC_V1         = 1
ERR_PERSONALITY_{UNAVAILABLE,CANCELLED,TIMEOUT,FAILED,STALE,DUPLICATE,BUSY,UNSUPPORTED}
```

In other words: the router, the supervision lifecycle, the memory-view *design*,
and the crossing *design* exist. What does not exist yet — and this proposal
does not pretend otherwise — is (a) a real foreign syscall corpus behind
`SYS_PERSONALITY_CALL`, (b) the *implementation* of the memory-view protocol
that M6B requires before any pointer-bearing call, and (c) the M9 crossing
codecs. Those are the work.

---

## 3. The crossing membrane — how "utilize" actually happens

If Section 0 is right that "fully utilize" means "make foreign resources
first-class without ambient authority," then the single most important part of
this whole effort is not any syscall table — it is the three-channel crossing
contract (M9). It deserves to be built *early and rigidly*, because it is what
prevents a personality from becoming an untyped RPC escape hatch, and it is what
lets one domain's resources be used by another.

**OValue channel — structural data.** Numbers, text, bytes, lists, maps, tables,
graphs, errors. Reuse the hosted `OValue` vocabulary but define a *versioned,
bounded kernel transport schema* with hard message-size / depth / node-count /
allocation quotas. The kernel never deserializes arbitrary Rust objects. A Linux
process's `write(2)` payload crossing to a native logging service is an OValue
crossing.

**Capability channel — live authority.** Files, shared memory, sockets, devices,
services, processes cross as *opaque transport references*; the kernel's atomic
transfer operation supplies the actual authority, always attenuated. This is the
channel that makes a foreign `fd` genuinely *usable* elsewhere: a Linux
personality's open file becomes an O-core file capability that a native service —
or an NT domain — can receive with strictly narrowed rights. This is
"utilization" in its strongest form, and it is only safe because of
attenuation-only transfer.

**Native-capsule channel — affinity-locked objects.** Some foreign objects
cannot be honestly normalized: a Mach port with complex right semantics, an NT
`APC` queue, a Binder death-recipient link. These cross as *capsules* tagged with
origin domain, personality, type, lifetime, and rehydration policy, defaulting to
**same-process or never**. The capsule is the honest admission that not
everything is portable — and encoding that dishonesty-refusal in the type system
is what keeps the whole system trustworthy.

The design rule (your invariant #10) is the thing to defend to the death:
**OValues are data, capabilities are authority, capsules preserve affinity, and
no transport silently converts one category into another.** A foreign kernel is
"fully utilized" exactly to the degree its resources can be expressed in these
three channels — and no further, on purpose.

---

## 4. Per-kernel map

Ordered by *structural difficulty*, which is not the same as popularity. The
right sequence is dictated by how stable and how capability-shaped each foreign
contract is.

### 4.1 Linux x86-64 — the correct first personality (M7–M8)

Linux is easiest for one structural reason: **its userspace ABI is deliberately,
famously stable** ("we do not break userspace"). The syscall boundary is the
contract — number in `rax`, args in `rdi, rsi, rdx, r10, r8, r9` — and a static
ELF64 binary needs almost nothing else. The object model is trivially
functorial: a Linux `fd` is a small integer index into a *personality-object
table* whose entries are O-core capabilities. `fd` never becomes a raw O-core
handle (invariant), it stays a personality object that *names* one.

Difficulty lives in a handful of syscalls, each of which maps onto machinery you
already have:

- `mmap`/`munmap`/`brk` → `address_space.oc` map/unmap + the memory-view
  protocol. Anonymous memory first; file-backed later.
- `clone`/`futex` → M2 scheduler + M3 endpoint wait epochs. A `futex` wait is
  strikingly close to a blocked TCB on an endpoint wake epoch — you already have
  the wake-once retry semantics (`ERR_IPC_RETRY`).
- `rt_sigaction`/signal delivery → your normalized trap-frame delivery path.
- `ioctl` → the compatibility swamp. It *must* stay corpus-pinned; an open
  `ioctl` surface is where "minimal Linux personality" quietly becomes "all of
  Linux." Return the documented error for anything off-corpus, never silently
  succeed.

Ship it as an immutable `personality/linux` package (M5), start with a pinned
static-binary corpus, and validate argument/errno/struct/signal/memory behavior
against a **native Linux oracle** before enabling the first pointer-bearing call.
M8 then adds multiple root filesystems — `linux[alpine]`, `linux[debian]` — over
*one* Linux personality with distinct namespaces. Debian vs Alpine are rootfs
compositions, not different personalities; keep that distinction crisp.

### 4.2 Android — Linux personality **plus a Binder capability service** (reframe)

The most useful thing I can tell you here: **Android is not a different kernel.**
It is the Linux kernel plus a userspace stack (bionic libc, the ART runtime) plus
a small set of kernel-adjacent primitives — chiefly the **Binder** IPC driver
(`/dev/binder`, an `ioctl`-driven transaction protocol), `ashmem`/`dmabuf` shared
memory, and the property service. So "contain the Android kernel" decomposes into
"the Linux personality (4.1) **+** a native O-core Binder service."

And Binder is almost isomorphic to what you already built. A Binder transaction
carries object references with kernel-managed reference counting and death
notification; handles are per-process and translated by the driver on every
cross-process hop. Read that again in your vocabulary: **Binder handles are
capabilities** — per-CSpace, generation-tagged, attenuated on transfer, with
death cleanup. Your `endpoint.oc` + `cap_transfer.oc` + IPC death-cleanup is a
better-typed Binder. So the plan is: expose a native `binderd` service over
endpoints; have the Linux personality translate the `/dev/binder` `ioctl`
surface into calls on it; map Binder's `flat_binder_object` reference passing
onto capability transfer; back `ashmem`/`dmabuf` with shared memory objects
(`memory_object.oc`). This is a genuinely elegant fit and, I suspect, a stronger
demonstration than raw Linux because it shows the capability model *absorbing* a
real-world object-IPC system rather than merely shimming POSIX.

### 4.3 macOS / XNU — Mach ports are capabilities, but the userland fights back (research-grade)

XNU is a hybrid: a Mach microkernel core + a BSD (POSIX) layer + IOKit. Two
syscall classes: BSD unix syscalls (positive numbers, ≈ the POSIX surface you
already serve for Linux — real reuse) and **Mach traps** (negative numbers:
`mach_msg`, `mach_port_*`, `vm_allocate`, …). On the Mach side the functor is
beautiful: `mach_port_t` send/receive rights are one of the origin points of
capability-OS lineage; `mach_msg` with port-right transfer is your endpoint
send/receive with capability transfer, almost line for line. If you only had to
implement Mach, XNU would rank *above* NT.

The difficulty is not the kernel model, it is everything Apple wraps around it:
binaries are **Mach-O**, not ELF (new loader); modern macOS requires the **dyld
shared cache** and code-signing/entitlements; and Apple actively breaks
compatibility release to release (Darling has pursued this for ~a decade and
still cannot run most GUI apps). There is also a hard **legal** edge: you cannot
redistribute Apple's frameworks or dyld cache — a user must supply them — so a
"macOS domain" is realistically a *Darwin* domain (the open-source core) plus
user-supplied Apple userland. Recommendation: treat a **Mach-trap + BSD subset
personality running Darwin/PureDarwin binaries** as the honest target, reuse the
Linux personality's POSIX work for the BSD half, and defer full macOS userland
indefinitely. This is a multi-year research line, not a slice.

### 4.4 Windows NT — the syscall boundary is the wrong seam (translate high, or subordinate)

NT is the hardest of the four, for a precise structural reason that inverts the
Linux argument: **the NT syscall boundary is deliberately *unstable*.** System
call numbers change between builds; Microsoft reserves the right and does it
routinely. The stable contract lives *higher* — at the **Native API** (`ntdll`'s
`Nt*`/`Zw*` functions) and above it the Win32 API (`kernel32`, `user32`,
`gdi32`). So a "Windows personality" cannot target the raw syscall table; it must
reimplement a large *userspace* surface at the ntdll/Win32 seam. That is Wine's
entire existence, and it is a very large, permanently-moving target. NT's object
model is also the richest: `HANDLE`s into the Object Manager with full
security-descriptor/ACL semantics, heavy ALPC use, and the registry as a
first-class namespace.

There is a lovely historical irony worth internalizing: **NT was *designed* as a
personality system.** The original NT ran Win32, POSIX, and OS/2 "subsystems" as
userland servers over the Executive's Native API — the exact shape you are
building, minus capabilities as the substrate. You are, in a real sense,
rebuilding Cutler's subsystem architecture on a capability core.

Honest recommendation: NT is where **full-kernel domain mode (M11) is the
realistic first answer.** Boot a real Windows kernel as `nt.kernel[0]` under the
VMM and integrate through a guest agent that speaks your three crossing channels.
In parallel, a *translate-high* path — run **Wine over the Linux personality**
(Wine-on-libOS) — gives you a surprising amount of Win32 for near-zero
NT-specific kernel work, because Wine already does the ntdll/Win32 translation
and only needs a POSIX host, which 4.1 provides. That hybrid (Wine userland +
Linux personality + native services) is likely the highest-leverage Windows story
before any native NT personality is justified.

### 4.5 Full-kernel domain mode — the universal fallback (M11)

For any kernel whose ABI is too unstable, too large, or too legally encumbered to
translate, subordinate it. A hardware-virtualization backend (KVM-class where
available) owns guest physical memory, vCPUs, interrupt injection, and
**paravirtual** devices only — no direct passthrough until IOMMU-backed DMA
isolation, reset, revocation, and hostile-device tests have their own evidence.
Crucially, crossings still go through the *same* OValue/capability/capsule
contract via a guest agent — never implicit host mounts or sockets. This keeps a
subordinate Linux/NT/XNU domain a first-class, *composable* citizen rather than a
walled-off VM, which is the whole point of doing it inside O-core instead of next
to it.

### 4.6 Summary matrix

| Target | ABI stability | Object model → O mapping | Recommended P1 | First honest deliverable |
|---|---|---|---|---|
| **Linux x86-64** | very high (stable syscalls) | `fd` → personality-object → capability | translate | pinned static-ELF corpus vs native oracle (M7) |
| **Android** | = Linux + Binder | Binder ref → **capability** (near-isomorphic) | translate + native `binderd` | Linux corpus + Binder transaction over endpoints |
| **XNU / Darwin** | low (Apple churn) | `mach_port` → **capability**; BSD → reuse Linux POSIX | translate (Mach+BSD subset) | Darwin binaries, Mach-trap subset; defer Apple userland |
| **Windows NT** | very low at syscalls; stable at ntdll/Win32 | `HANDLE`/Object Manager → capability + capsule | subordinate (M11) *or* Wine-on-Linux hybrid | boot `nt.kernel[0]` in VMM, or Wine over Linux personality |
| **Anything else** | — | — | subordinate | guest agent over the three crossing channels |

---

## 5. The real answer to "*all* kernels at once": composition (M9)

Hosting one foreign kernel is a genre with three shipping incumbents. The claim
worth making — and the one your architecture is distinctively built for — is
about *simultaneity and composition*, and it follows directly from the functor
framing in Section 1.

Because every personality `P_i` is a functor into the **same** codomain O, a
value or resource can be routed *through several of them in one typed graph*, and
the crossings between them are mediated by the M9 channels. Concretely, the
credible flagship demonstration is a single `ExecutionPlan`-style graph in which:

1. a native O-Domain produces a structural **OValue**;
2. it crosses into `linux[alpine]`, consumed by a pinned static binary via the
   Linux personality (`P_linux`);
3. that binary's output file crosses as an **attenuated capability** into a
   second domain (native service, or `nt` domain via agent);
4. every personality **state/effect transition is recorded** as a distinct graph
   node, exactly as your HGraph already records resource/actor/completion nodes
   for hosted backends; and
5. revoking any upstream domain or capability **deterministically fails
   downstream operations** — and a crash in any one personality service leaves
   the others running (M3 containment, already gated at scalar scope).

That is not "a kernel that contains kernels." It is a **capability-mediated
composition substrate in which foreign kernels are interchangeable personalities
over one invariant request/authority structure.** The container metaphor
undersells it; the accurate metaphor is a *common categorical target* into which
each foreign kernel is a functor, with the crossing channels as the natural
transformations between them. The novelty, and the paper, is the composition —
not any single emulation.

This also reframes "fully utilize" one final time: a foreign kernel is fully
utilized when its resources are *reachable as ordinary nodes in the O-Domain
graph* — schedulable, attenuatable, revocable, composable with every other
domain — rather than trapped behind an opaque boundary. The three channels are
the exact measure of how much utilization is honestly available.

---

## 6. Non-claims and blockers (kept in your CLAIMS register style)

- Nothing here is implemented by virtue of a type, constant, or name existing.
  `personality.oc` naming `PERSONALITY_NATIVE`/`PERSONALITY_TEST` and
  `native_abi.oc` reserving `SYS_PERSONALITY_CALL = 14` are *reserved seams*, not
  Linux support. M7–M11 are all **planned**.
- No pointer-bearing foreign syscall may be admitted until the **M6B**
  implementation of `PERSONALITY_MEMORY_VIEW.md` passes its full gate: no service
  ever receives a raw foreign pointer; every foreign address is interpreted in
  the target's generation-tagged address space and exposed only as a bounded,
  request-scoped, revocable view; crash/restart/cancel/timeout/stale-reply close
  every view and wake each dependent thread at most once.
- "Compatibility" claims must always name the **binary corpus, architecture,
  syscall slice, and execution mode** tested against a native oracle. "Runs
  Linux" is not a claim; "runs *this* pinned static corpus with *these* syscalls,
  errno-exact vs oracle" is.
- The `ioctl` / driver / GUI / dynamic-loader long tail is unbounded and must
  stay explicitly corpus-pinned. Off-corpus behavior fails closed with the
  documented error.
- NT's syscall ABI instability makes a native syscall-level NT personality a
  poor early target; XNU's Mach-O + dyld cache + code-signing + Apple churn plus
  the legal non-redistribution of Apple userland make full macOS translation
  research-grade. Both are honestly addressed first via subordination (M11) or a
  Mach/BSD-subset-over-Darwin slice.
- Full-kernel mode must not become a hole in the security model: guest-agent
  crossings only, paravirtual devices first, hard quotas, **no** ambient host
  mounts or sockets, and **no** direct passthrough before IOMMU-backed isolation
  and hostile-device evidence.

---

## 7. The next PR — a concrete M7 slice-1 on top of M6A

The smallest change that turns the reserved seam into a real (tiny) foreign
personality, chosen to reuse everything M4–M6A already gate:

1. **Identity.** Add `PERSONALITY_LINUX` to `personality.oc` and extend
   `is_supported`. Do *not* wire it into privileged policy.
2. **Service.** Fork `m6_personalityd.oc` into a `linux_personalityd` package
   ELF (built by a `build-m7-artifacts.sh` alongside the M6 artifacts), activated
   as immutable `personality/linux` through the M5 supervisor.
3. **Loader.** Reuse the M4 static-ELF/OVFS loader path to load *one pinned
   static x86-64 Linux binary* into an isolated W^X address space. Freestanding /
   `-nostdlib` first, so the corpus needs almost no syscalls.
4. **Object table.** Add a per-process **personality-object table** mapping Linux
   `fd` → O-core capability. `fd` stays a personality object; the raw handle
   never crosses the ABI.
5. **Corpus.** Implement, behind `SYS_PERSONALITY_CALL`, the smallest useful
   Linux syscall set — `write`, `exit_group`, `brk`, `mmap`(anon), `read`,
   `clock_gettime` — returning Linux-exact errno for anything off-corpus. Keep
   every one of these *scalar-or-bounded-copy* so you stay inside current
   memory-view capabilities and do **not** yet need M6B pins.
6. **Oracle gate.** Add `smoke-linux-personality-qemu.sh` (next probe mode) that
   runs the pinned binary load-to-exit and diffs argument/errno/exit behavior
   against a recorded **native Linux oracle** trace; assert that a
   personality-service fault cannot stop an unrelated native domain (extend the
   M6A containment assertion).

That single slice is defensible under your existing claim discipline — it names a
one-binary corpus and a six-syscall slice — and it exercises the entire spine
(package activation → loader → personality router → object table → oracle diff →
containment) that every later kernel reuses. After it lands, the highest-leverage
follow-ups are, in order: **(a)** implement M6B memory views (unlocks
pointer-bearing syscalls and thus real programs), **(b)** the Binder `binderd`
service (unlocks Android and *demonstrates the capability model absorbing a real
object-IPC system*), and **(c)** the first M9 OValue crossing between a native
and a Linux domain (unlocks the composition thesis in Section 5).

---

*Written against `ocore/runtime/x86_64/` (`native_abi.oc`, `personality.oc`,
`personality_rpc.oc`, `cap_transfer.oc`, `endpoint.oc`, `address_space.oc`,
`memory_object.oc`), `ocore/kernel/` gates, and `docs/ODOMAIN_PLAN.md`,
`docs/PERSONALITY_MEMORY_VIEW.md`, `docs/CLAIMS.md`.*
