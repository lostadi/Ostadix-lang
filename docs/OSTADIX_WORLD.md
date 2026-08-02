# Ostadix World: Full-Stack Machine-Constructor Roadmap

## The ambitious native path from the current Ostadix kernel to an elastic eight-node computer

**Status:** normative native Alpha constitution and implementation program,
version 1. The machine-readable qualification registry is
[`evidence/world_alpha_gates.toml`](../evidence/world_alpha_gates.toml).

**Primary target:** a physically distributed computer whose identity is constituted by a governed World rather than by a chassis.

> A computer is not a box. A computer is a governed structure of computational resources.

This document deliberately does **not** optimize for the quickest hosted demonstration. It defines the direct path toward the machine Ostadix is intended to become: multiple physical machines booting O-core, contributing typed resources to one replicated World, reusing foreign kernels as capability-governed hardware and compatibility organs, exposing a Linux-compatible Debian environment, and continuing coherently as membership changes.

Hosted Linux implementations remain valuable as simulators, differential oracles, protocol fuzz targets, and development consoles. They do not satisfy the native release gates in this roadmap.

---

# 1. Correction to the previous roadmap

The previous roadmap was coherent but strategically too conservative. It made a Linux-hosted Governor and Linux-hosted node daemons the critical path, then postponed the hardest and most identity-defining work:

- AArch64 support for USB-sized single-board computers;
- multiprocessor O-core execution;
- a replicated Governor;
- a real 9P-derived World namespace;
- booting a real foreign Linux kernel as a contained KernelWorld;
- assigning or transplanting a real hardware driver;
- a Linux userspace ABI broad enough for a Debian root filesystem;
- distributed execution and recovery on physical O-core nodes; and
- a federated accelerator service.

That plan would have proved the fabric semantics while leaving the actual operating system as a substrate swap for later. Ostadix is not merely a substrate-neutral orchestration protocol. Its strongest proposition is that O-core becomes the constitutional layer beneath foreign kernels, distributed resources, and familiar user environments.

The revised strategy is therefore:

> Advance the hard native workstreams in parallel, force them to meet at explicit integration gates, and forbid a hosted implementation from masquerading as completion.

This is not a rejection of sequencing. It is a rejection of sequencing that quietly turns the destination into an optional sequel.

---

# 2. The non-negotiable end state

## 2.1 Ostadix World Alpha qualifying gate

The first release allowed to call itself **Ostadix World Alpha** must demonstrate all of the following in one integrated system:

1. **At least three physical machines boot O-core as the sovereign kernel.**
   - At least one target must be AArch64.
   - The final showcase target is an eight-node AArch64 single-board-computer fabric.
   - Linux-hosted nodes may participate in mixed-world compatibility tests but do not count toward the native minimum.

2. **O-core runs with symmetric multiprocessing on multicore hardware.**
   - More than one CPU core executes ordinary kernel and user work.
   - Capability operations, mappings, IPC, world lifecycle transitions, and task commitment preserve defined linearization points under concurrency.

3. **The Governor is logically singular and physically replicated.**
   - Three Governor replicas maintain one authoritative replicated log.
   - Loss of one replica does not destroy the World.
   - A minority partition cannot mint global authority or commit globally visible results.

4. **The World has a live 9P-derived global namespace.**
   - Membership, resources, services, tasks, objects, personalities, and events are visible under `/world`.
   - Per-process namespace composition is supported.
   - Names identify resources, while capabilities authorize their use.

5. **A real foreign Linux kernel runs as a contained KernelWorld.**
   - It boots from a pinned kernel image and initramfs.
   - A bounded Ostadix guest agent performs health negotiation.
   - Its generation and authority are governed by O-core.

6. **A real physical device is controlled through the foreign-kernel machinery.**
   - The device is resettable and bounded.
   - DMA windows, interrupts, revocation, teardown, and replacement are tested.
   - O-core consumes the resulting service through a typed capability rather than through a guest pointer or an ungoverned host API.

7. **A Linux-compatible personality runs dynamically linked programs from a pinned Debian root filesystem.**
   - A shell, core file utilities, process creation, signals, virtual memory, pipes, terminal I/O, and networking work.
   - `dpkg` works before the Alpha gate.
   - `apt` over the World network service is part of the Alpha gate, not an indefinite postscript.

8. **The HGraph becomes a distributed execution model.**
   - A project lowers to a logical HGraph.
   - A separate deployment plan assigns operations, artifacts, capabilities, and affinity-bound capsules to nodes.
   - Node loss causes defined withdrawal, fencing, checkpoint recovery, or explicit task loss according to policy.

9. **The World provides aggregate, locality-aware data capacity without pretending to provide uniform coherent RAM.**
   - Immutable artifacts, streams, sharded objects, replicated objects, checkpoints, and accelerator buffers are first-class.
   - Their locations and movement costs remain inspectable.

10. **At least one accelerator is exported as a governed service.**
    - The initial portable target should be a Vulkan-compute or SPIR-V service hosted by a Linux driver domain.
    - Buffers remain node-local unless explicitly transferred or replicated.

11. **Elasticity is visible and operational.**
    - A node joins and its resources appear.
    - A node disappears and its resources are withdrawn.
    - Stale capabilities and stale task attempts fail.
    - Unaffected work continues.
    - A returning node receives a fresh generation.

12. **Every major claim is tied to executable evidence.**
    - QEMU evidence, x86 hardware evidence, AArch64 hardware evidence, and multinode evidence are classified separately.
    - Each gate records both what it proves and what it does not prove.

## 2.2 What does not count as completion

None of the following alone qualifies as Ostadix World Alpha:

- several Linux machines running an Ostadix daemon;
- a single authoritative Governor;
- a mock 9P tree over in-memory objects;
- a synthetic guest that never boots Linux;
- a virtual device with no physical DMA or interrupt lifecycle;
- a static BusyBox-style binary with a tiny pinned syscall corpus;
- a Debian container running above Linux;
- a distributed task runner that does not use O-core capabilities and generations;
- a throughput benchmark that hides locality and failure semantics; or
- a diagram that claims aggregate RAM as uniform local memory.

These may be valid development gates. They are not the destination.

---

# 3. Architectural constitution

## 3.1 One computer means one governed World

A World is not defined by a motherboard. It is defined by the tuple

```text
World = (
    identity,
    epoch,
    membership,
    namespace,
    authority_graph,
    resource_graph,
    execution_graph,
    object_graph,
    failure_policy,
    evidence_log
)
```

A node contributes resources to the World only after admission. A resource remains physically local even when it becomes globally nameable and schedulable. The Governor reconciles the logical unity of the World with the nonuniform physics of its members.

The system must always be able to answer:

- Which node owns this resource?
- Which generation created this handle?
- Which authority path permits this operation?
- Which data must move before the operation can run?
- What survives if this member disappears?
- What result is globally committed, and under which Governor term and attempt generation?

## 3.2 The three crossing kinds remain constitutional

Every boundary crossing is classified as exactly one of the following:

### OValue -- portable data

An OValue is canonical data whose representation can be transmitted without carrying ambient authority or hidden process identity. It may be copied, hashed, stored, replayed, and rendered into another evaluator subject to a versioned schema.

### Capability -- transferable authority

A capability authorizes operations on a governed object. It crosses only through authenticated delegation and only with equal or reduced rights. Its identity includes generation and issuer context. A serialized description is never itself authority.

### Capsule -- explicit affinity

A capsule represents state whose semantics depend on an origin node, process, domain, device, address space, or lifetime. It cannot silently become portable. It may be pinned, proxied, checkpointed by an adapter, or refused.

The implementation position is conservative and epistemic:

> Unknown portability defaults to capsule. A tested transport may later promote a particular capsule class to a portable value or attenuable capability, but no transport invents that promotion implicitly.

## 3.3 The mesh is canonical; the Linux face is a projection

Ostadix should provide two simultaneous truths:

- a familiar Linux-compatible environment in which ordinary users can work; and
- an inspectable World view that exposes membership, locality, authority, generations, and failures.

The Debian personality may make the World convenient. It must not make the World dishonest.

A user can enter:

```text
o shell --world desk --personality debian
```

and receive a conventional shell, while `/world` still reveals that a GPU belongs to node 6, an artifact has replicas on nodes 1 and 4, and a process is pinned to node 3 by a capsule.

## 3.4 The Governor consistency model is fixed now

The authoritative plane uses a three-replica Raft-style consensus group.

- Membership changes, World epochs, capability roots, resource admission, global task commitment, and namespace mutations are linearizable through the replicated log.
- Ordinary telemetry may be served from a recent snapshot and labeled with its log index.
- Clocks assist failure detection but never establish authority by themselves.
- Every globally committing operation carries a fencing tuple containing at least Governor term, log index or World epoch, and attempt generation.
- The majority partition remains the authoritative World.
- A minority partition enters **island mode**. It may continue explicitly local, noncommitting work, preserve local state, and serve local diagnostics. It may not mint globally valid capabilities, mutate the global namespace, or commit globally visible results.
- Rejoining does not resurrect the old node generation. The node is readmitted and stale work remains fenced.

This gives Ostadix graceful shrink without allowing split-brain authority.

## 3.5 The four planes are separate by design

```text
Namespace and control plane
    9P2000 / 9P2000.L-derived names, mounts, inspection, control

Authority plane
    generation-bound Ostadix capabilities and delegation proofs

Execution plane
    Governor operations, HGraph deployment, attempts, checkpoints, commits

Bulk data plane
    chunked content transfer, streams, shared rings, DMA, accelerator buffers
```

9P supplies the grammar of composition. It does not have to carry every tensor byte, disk block, or packet.

## 3.6 The memory model is aggregate and explicit, not transparent DSM

Ostadix World does not promise a uniform shared-memory machine over Ethernet. It provides globally governed capacity through explicit object kinds:

- `OArtifact` -- immutable, content-addressed, replicable data;
- `OBlob` -- immutable chunked data with streaming transfer;
- `ORegion` -- mutable, leased, node-owned memory exposed through explicit operations;
- `OStream` -- bounded producer/consumer transport;
- `OShardSet` -- partitioned data with placement metadata;
- `OTensor` -- accelerator-local or node-local multidimensional buffer;
- `OCheckpoint` -- restartable execution state;
- `OCapsuleRef` -- a capability-addressed proxy to affinity-bound state.

An ordinary pointer is never silently promoted into a remote pointer. Computation is moved toward data whenever possible. Data movement remains represented in the deployment graph and receipt.

---

# 4. Current repository baseline and the actual gaps

The present archive already contains a surprisingly large portion of the semantic skeleton. The roadmap should extend these structures rather than create an unrelated cluster stack beside them.

| Existing substrate | Current anchor | What it already establishes | Ambitious gap |
|---|---|---|---|
| Polyglot execution | `OIR`, `ExecutionPlan`, `src/hgraph/` | explicit values, completions, resource states, actor state, constraints | distributed placement, native World identities, cross-node effects |
| Effects | `src/effects.rs` | host paths, environment, network, services, actors, scope | precise World, node, domain, object, capability, device, and locality resources |
| Project execution | `src/project/` | bundle lifting, routes, isolation, cancellation, artifacts, equivalence | project-to-HGraph construction and remote deployment |
| Hosted lifecycle | `src/live_system/` | package generations, health gating, rollback, stale-bearer denial | native replicated lifecycle and cross-node supervision |
| Hosted authority bridge | `src/ocore/capability_bridge.rs` | metadata is not authority; live bearer binds to generation-tagged handle | authenticated network delegation and native capability transport |
| KernelWorld model | `src/kernel_world.rs` | identity, generation, lifecycle, request and terminal-result discipline | shared identity with HGraph and replicated World state |
| O-core kernel | `ocore/runtime/x86_64/` and `ocore/kernel/` | memory, processes, scheduler, IPC, capabilities, packages, personality and world slices | architecture-neutral core, AArch64, SMP, real networking, dynamic allocation |
| Linux personality | `linux_personality.oc`, Mode 25 corpus | exact bounded static Linux ELF and pinned syscall behavior | dynamic ELF, glibc ABI, broader syscalls, rootfs, package manager |
| 9P composition | Mode 26 and `m7_linux_9pd.oc` | exact bounded 9P2000 exchange and lifecycle evidence | live network transport, namespace server, mounts, 9P2000.L semantics |
| Foreign kernel execution | KernelWorld Mode 23/SVM scaffolding | admission, execution shape, synthetic guest, bounded virtual endpoint | real Linux boot, guest agent, shared queues, physical device ownership |
| Evidence | `docs/CLAIMS.md`, `evidence/gates.toml`, smoke scripts | unusually strong claim/non-claim discipline | multinode, AArch64, consensus, hardware-IOMMU, and fault-injection gates |

Two structural seams deserve immediate treatment:

1. `HostWorld@N` in the hosted execution graph and `KernelWorldIdentity { generation: N }` in the kernel are not yet one governed identity system.
2. The project HGraph operation kinds exist, but the current lowering does not construct the project-level graph that the distributed Governor needs.

The first makes native execution epistemically continuous with the planner. The second turns the planner into the machine's live model rather than a language-only artifact.

---

# 5. Program structure: parallel workstreams, converging gates

The work should be organized as a **program of fifteen parallel workstreams**. No one workstream is allowed to run indefinitely as an isolated research branch. Each must feed a shared integration gate.

The governing rule is:

> A subsystem is not considered advanced merely because it has more code. It advances when another subsystem can depend on its precise contract and a failure gate confirms that dependency.

The workstreams are:

1. shared World semantics and identity;
2. architecture-neutral O-core compiler and AArch64 backend;
3. architecture-neutral kernel, SMP, and memory safety;
4. native networking and secure transport;
5. replicated Governor;
6. World namespace and 9P service fabric;
7. real foreign-kernel execution;
8. Linux driver domains and physical-device lifecycle;
9. Linux ABI and Debian personality;
10. distributed HGraph execution;
11. distributed objects and storage;
12. accelerator fabric;
13. security, enrollment, and attestation;
14. observability, command surface, and developer environment; and
15. evidence, formal models, and adversarial testing.

The following sections define each workstream in concrete terms.

# 6. Workstream A -- unify World semantics, identity, and receipts

## Objective

Create one vocabulary that is shared by the hosted reference implementation, the O compiler and HGraph, O-core services, foreign KernelWorlds, network messages, and evidence receipts.

## Concrete construction

Introduce versioned identities at the language/runtime boundary:

```text
WorldId
WorldEpoch
GovernorTerm
GovernorLogIndex
NodeId
NodeGeneration
DomainId
DomainGeneration
ProcessId
ProcessGeneration
ResourceId
ResourceGeneration
ObjectId
ObjectVersion
CapabilityId
LeaseId
TaskId
AttemptGeneration
CheckpointId
ReceiptId
```

Create a canonical, architecture-independent wire schema. The Rust implementation may remain the schema oracle, but the schema must have a native `.oc` implementation and byte-for-byte cross-language tests.

Proposed hosted layout:

```text
src/world/
    mod.rs
    identity.rs
    protocol.rs
    event.rs
    resource.rs
    authority.rs
    object.rs
    task.rs
    receipt.rs
    codec.rs
```

Proposed native layout:

```text
ocore/world/
    identity.oc
    protocol.oc
    event.oc
    resource.oc
    authority.oc
    object.oc
    task.oc
    receipt.oc
    codec.oc
```

Extend `ResourceKey` beyond ambient host categories with precise governed resources:

```text
WorldState(WorldId)
GovernorState(WorldId)
NodeState(NodeId, NodeGeneration)
DomainState(DomainId, DomainGeneration)
ProcessState(ProcessId, ProcessGeneration)
ResourceState(ResourceId, ResourceGeneration)
ObjectState(ObjectId, ObjectVersion)
CapabilityState(CapabilityId)
NamespaceState(WorldId, WorldEpoch)
ArtifactState(ObjectId)
DeviceState(ResourceId, ResourceGeneration)
AcceleratorState(ResourceId, ResourceGeneration)
```

Retain `HostWorld` only for genuinely opaque hosted operations. When an O program runs under O-core and all effects cross governed interfaces, the execution graph should not need the ambient `HostWorld` umbrella.

Freeze a versioned OValue core and add explicit extension envelopes. OValue must not become a bag into which every subsystem dumps private variants. The core should contain scalars, text, bytes, lists, records, maps with canonical key rules, tagged sums, code references, object references, and error values. Authority stays outside OValue.

Create one canonical receipt structure covering:

- source, bundle, package, and logical-HGraph digests;
- World identity, epoch, Governor term, and log index;
- node, domain, process, and attempt generations;
- delegated capability identities and attenuated rights;
- object inputs, outputs, replicas, and transfers;
- capsule affinities;
- effects and resource transitions;
- placement decisions and rejected alternatives;
- checkpoints and recovery actions;
- terminal result and commit fencing; and
- evidence gate identity.

O-Git should consume these receipts rather than infer semantics from special comments.

## Acceptance gate A

A single logical project executed through the hosted oracle and through O-core must emit receipts that decode to the same canonical structure. A stale `NodeGeneration`, `DomainGeneration`, `AttemptGeneration`, or `ObjectVersion` must fail before execution or commit. `o plan --grounding` must distinguish explicit OValues, capabilities, capsules, governed resources, and residual `HostWorld` dependencies.

---

# 7. Workstream B -- architecture-neutral compiler and first-class AArch64

## Objective

Turn the current x86_64-specific O-core compiler into a multi-target systems compiler capable of producing the native kernel and user services for the actual SBC class Ostadix is meant to inhabit.

## Concrete construction

Refactor the current monolithic `src/ocore/codegen.rs` into:

```text
src/ocore/codegen/
    mod.rs
    target.rs
    machine_ir.rs
    legalize.rs
    liveness.rs
    regalloc.rs
    frame.rs
    emit.rs
    x86_64/
        abi.rs
        isel.rs
        encode.rs
        reloc.rs
    aarch64/
        abi.rs
        isel.rs
        encode.rs
        reloc.rs
```

The target-neutral Machine IR should represent:

- virtual registers and register classes;
- integer, floating-point, vector, address, and condition values;
- explicit calls and calling conventions;
- atomic operations and memory ordering;
- basic blocks, branches, phi lowering, and critical-edge handling;
- stack objects and alignment;
- relocatable symbols and constant pools;
- trap, interrupt, and syscall entry conventions; and
- target feature requirements.

Implement a real register allocator. A linear-scan allocator is acceptable for the first integrated backend, provided spilling, callee-save handling, fixed-register constraints, and deterministic allocation are tested. The stack-spill backend should remain as a differential oracle and emergency fallback, not the production endpoint.

Complete currently weak or absent language lowering needed by kernels and driver services:

- indirect calls and function pointers;
- floating-point scalar operations;
- AArch64 NEON/SIMD value classes;
- atomics with explicit memory order;
- volatile and MMIO-safe access;
- packed and aligned structure layout;
- variadic or bounded foreign-call support where required;
- thread-local storage;
- unwind-free error paths suitable for freestanding code; and
- architecture-independent inline-assembly constraints or named intrinsics.

Implement the AArch64 procedure-call standard and ELF relocations needed by freestanding kernels and user programs, including page-relative addressing, branches, GOT-like references where used, and relocation range diagnostics.

Add target triples such as:

```text
x86_64-unknown-ocore
x86_64-unknown-ocore-user
aarch64-unknown-ocore
aarch64-unknown-ocore-user
```

Build the same semantic corpus for both targets and compare OValue-level results. Rebuild artifacts twice and require byte-identical output when all inputs and tool versions are pinned.

## AArch64 execution ladder

1. Compile a freestanding return-value corpus.
2. Emit a valid AArch64 relocatable ELF object.
3. Link a minimal image and execute under `qemu-system-aarch64 -machine virt`.
4. Bring up PL011 serial, the generic timer, PSCI calls, GICv3, and virtio-mmio discovery.
5. Boot the full O-core kernel under AArch64 QEMU.
6. Chainload O-core through UEFI or U-Boot on a physical reference board.
7. Parse the device tree blob and publish discovered platform resources as typed kernel objects.
8. Run native O user processes and the World protocol codec on physical AArch64 hardware.

## Acceptance gate B

The same O-core kernel source, apart from architecture modules, boots on x86_64 and AArch64. Both targets run process, IPC, capability attenuation, package loading, KernelWorld lifecycle, and World protocol tests. A physical AArch64 board boots O-core and emits a signed hardware evidence record.

---

# 8. Workstream C -- architecture-neutral O-core, SMP, and protected memory

## Objective

Transform the fixed-capacity, single-CPU x86_64 research kernel into a multi-architecture, multicore capability kernel with the isolation primitives required to govern foreign kernels and physical devices.

## Kernel decomposition

Refactor `ocore/runtime/x86_64/` into architecture-neutral and architecture-specific strata:

```text
ocore/runtime/core/
    capability/
    cspace/
    process/
    thread/
    scheduler/
    ipc/
    memory/
    mapping/
    package/
    supervisor/
    personality/
    kernel_world/
    namespace/
    world/

ocore/runtime/arch/x86_64/
    boot/
    traps/
    interrupts/
    paging/
    timer/
    virtualization/
    iommu/

ocore/runtime/arch/aarch64/
    boot/
    exceptions/
    gic/
    paging/
    timer/
    virtualization/
    smmu/
```

The refactor must not become a flag-day rewrite. Move one abstraction at a time while preserving the existing QEMU gate transcripts.

## Dynamic resource management

Replace fixed global arrays on critical paths with capability-accounted allocators:

- physical page allocator with zones and reservations;
- kernel heap or typed slab allocators;
- dynamic process, thread, endpoint, mapping, capability, and service tables;
- bounded quotas per process and domain;
- explicit allocation-failure propagation; and
- reclamation gates for every object class.

Retain hard upper bounds where they are part of the security policy, but make them policy rather than accidental static storage.

## SMP strategy

Begin with correctness, not premature lock granularity.

1. Add per-CPU state, CPU identity, local interrupt state, kernel stacks, and run queues.
2. Introduce one explicit `KernelLinearizationLock` for operations whose current proofs rely on a single operation owner.
3. Associate each committed kernel transition with a monotonically advancing linearization epoch.
4. Bring up secondary cores through AP startup on x86_64 and PSCI on AArch64.
5. Add interrupt-safe spinlocks, wait queues, atomics, and memory barriers with architecture-specific implementations.
6. Implement cross-core rescheduling and interprocessor interrupts.
7. Implement TLB shootdown with generation-tagged address spaces.
8. Make capability transfer, revocation, endpoint send/receive, mapping, process destruction, domain replacement, and KernelWorld lifecycle linearizable under contention.
9. Only after the global-lock gates pass, partition locks by subsystem and use lock-order assertions.
10. Add race detectors or deterministic stress schedulers to force rare interleavings.

The existing rendezvous and rollback discipline should be used as a proof template: each concurrent operation must have an identifiable commit point, and failure before that point must leave no partially published authority.

## Virtual memory and isolation

Implement:

- demand paging;
- copy-on-write process creation;
- guard pages and non-executable mappings;
- address-space randomization where compatible with deterministic evidence;
- pinned memory objects for device and foreign-kernel exchange;
- explicit cache-maintenance operations on noncoherent architectures;
- stage-2 translation or nested paging for contained guests;
- IOMMU domains on x86_64;
- SMMU domains on capable AArch64 targets;
- interrupt remapping where available; and
- a first-class reset/revocation state machine for assigned devices.

## Acceptance gate C

A multicore x86_64 machine and a multicore AArch64 machine execute kernel stress gates with concurrent capability transfer, IPC, page mapping, process teardown, World admission, and foreign-domain replacement. No stale generation regains authority. The same tests pass under forced preemption and allocation failure.

---

# 9. Workstream D -- native networking and secure World transport

## Objective

Give O-core a native route into the World without making Linux the sovereign host, while allowing Linux driver domains to provide richer network devices later.

## Bootstrap transport

Implement two independent bootstrap paths so that the architecture is not trapped behind one driver strategy:

1. **Virtual/reference path:** virtio-net under x86_64 and AArch64 QEMU.
2. **Physical/reference path:** one well-documented Ethernet or USB Ethernet controller with a minimal native O-core driver.

The native driver need not reproduce a complete Linux network stack. It must reliably provide the control-plane transport needed to discover, enroll, and supervise nodes.

## Native network stack

Implement a bounded but real stack:

- Ethernet framing;
- ARP and IPv6 neighbor discovery;
- IPv4 and IPv6;
- ICMP diagnostics;
- UDP;
- TCP sufficient for control and package transport;
- DHCP and static configuration;
- DNS client service; and
- packet-buffer capability accounting.

Use a service architecture so protocol processing can move out of the privileged kernel where possible.

## World transport protocol

Build a multiplexed authenticated transport with:

- message framing and canonical encoding;
- stream identifiers;
- flow control and bounded queues;
- request cancellation and deadlines;
- replay protection;
- peer identity binding;
- World, node, and generation binding;
- separate control, execution, event, and bulk channels;
- reconnect and resumable object transfer; and
- explicit backpressure visible to the scheduler.

A transport session is not a capability. It carries capability-bearing requests whose authority is validated independently.

## Acceptance gate D

Three physical O-core nodes discover one another, establish mutually authenticated channels, exchange canonical World messages, transfer a content-addressed object, survive connection loss, and reject replayed messages from an old node generation.

---

# 10. Workstream E -- the replicated Governor

## Objective

Build the constitutional authority of Ostadix World as a native replicated service, not a single-process scheduler.

## Reference and native implementations

Maintain two implementations against one executable state-machine specification:

- a Rust reference Governor for exhaustive simulation, model-based testing, and protocol fuzzing;
- a native O-core Governor service written in `.oc` and deployed as three replicas.

The Rust implementation is an oracle. The native implementation is the release implementation.

## Replicated state

The authoritative log includes:

- World creation and epoch changes;
- Governor replica membership;
- node enrollment, admission, quarantine, withdrawal, and generation changes;
- resource publication and withdrawal;
- global namespace mutations;
- capability roots, delegation records, revocation epochs, and issuer generations;
- object metadata, replica sets, and ownership leases;
- task creation, attempt placement, checkpoint publication, and commit;
- personality and service generations;
- package and policy digests; and
- evidence and audit checkpoints.

Do not replicate high-volume data through the consensus log. Replicate metadata and content hashes; transfer bulk objects through the data plane.

## Core protocols

Implement:

- leader election;
- log replication;
- durable term and vote state;
- snapshots and compaction;
- membership reconfiguration;
- read-index or equivalent linearizable reads;
- lease-backed resource liveness without making clocks a safety primitive;
- fencing tokens for tasks, objects, and capabilities;
- idempotent client request IDs;
- exactly-one global task commit;
- disaster recovery from snapshots plus retained object metadata; and
- explicit island mode.

## Failure semantics

A node can be:

```text
unknown -> enrolling -> admitted -> healthy -> suspect -> withdrawn
                                      |                     |
                                      +---- draining -------+
withdrawn -> reenrolling -> admitted(new generation)
```

A task attempt can be:

```text
created -> placed -> running -> checkpointed -> completed -> committed
                     |             |             |
                     +-> failed ---+             +-> fenced
                     +-> lost --------------------+
```

Completion is not commitment. Only the current attempt generation under the current fencing context can commit.

## Acceptance gate E

Three native Governor replicas on three O-core nodes elect a leader, admit additional nodes, survive one replica loss, reject a stale leader, snapshot and restore their state, and preserve exactly-one task commitment during message duplication, delay, reordering, and partition.

---

# 11. Workstream F -- WorldFS and the 9P-derived namespace

## Objective

Turn the bounded Mode 26 exchange into the live compositional namespace through which the World can be inspected and controlled.

## Protocol ladder

1. Preserve the exact 9P2000 oracle already present.
2. Implement a reusable codec with bounded allocation and adversarial length checks.
3. Implement the core request set: version, auth, attach, walk, open, create, read, write, clunk, remove, stat, and wstat.
4. Implement the Linux-oriented operations required from 9P2000.L, including getattr, setattr, readdir, readlink, symlink, mkdir, rename, unlink, fsync, lock, and xattr operations as needed by the Linux personality.
5. Add an Ostadix capability-binding extension, versioned separately, in which opening a control or service endpoint negotiates a generation-bound capability or data channel.
6. Support local IPC, shared-memory, and network transports under the same server contract.

## Namespace model

The committed World view should expose at least:

```text
/world/
    identity
    epoch
    governors/
    members/
        <node-id>/
            generation
            health
            topology
            resources/
            domains/
            evidence/
    resources/
        cpu/
        memory/
        storage/
        accelerators/
        devices/
        services/
    processes/
    tasks/
    objects/
    artifacts/
    checkpoints/
    personalities/
    packages/
    policies/
    events/
    claims/
```

Every namespace object has a stable logical identity and a current generation or version. A fid is bound to both. Replacement never revives an old fid.

Implement Plan 9-style per-process namespace composition:

- mount;
- bind before/after/replace;
- unmount;
- private namespace cloning;
- inherited namespace snapshots; and
- policy-constrained namespace templates for personalities.

Names remain separate from authority. Walking to `/world/resources/accelerators/gpu0` does not grant permission to submit work.

## Cache and churn semantics

Implement:

- lease- or epoch-tagged directory caches;
- invalidation events on membership and generation changes;
- negative-cache expiry;
- reconnect rules;
- partial reads with bounded consistency metadata; and
- stale-fid errors that identify the replacement generation without silently rebinding.

## Acceptance gate F

A process on one physical O-core node mounts WorldFS from the replicated Governor view, composes a private namespace containing services from multiple nodes, loses one service node, observes precise withdrawal, and cannot use an old fid or capability after replacement. Unrelated mounts remain live.

# 12. Workstream G -- boot a real foreign Linux KernelWorld

## Objective

Replace the synthetic guest evidence with a real, pinned Linux kernel that O-core admits, boots, supervises, revokes, replaces, and consumes through typed exports.

## Foreign-kernel package

Define a content-addressed package containing:

```text
kernel image
initramfs
kernel configuration
command line
optional device tree
expected architecture
required virtualization features
memory and vCPU quotas
allowed physical-device classes
allowed shared-memory views
allowed interrupt routes
exported service schemas
guest-agent protocol version
health deadline
reset and replacement policy
all source and binary digests
```

The manifest never grants authority. Admission resolves each requested resource to a live capability and denies anything not explicitly grounded.

## Boot path

For x86_64:

- complete the existing AMD SVM path and add Intel VMX when the architecture is stable;
- construct guest physical memory through explicit memory objects;
- load a pinned Linux `bzImage` or EFI-stub image;
- provide boot parameters and initramfs;
- establish nested page tables;
- inject interrupts through governed virtual interrupt state; and
- expose only explicitly declared paravirtual devices.

For AArch64:

- execute the guest at EL1 under O-core at EL2;
- construct stage-2 page tables;
- provide PSCI and a minimal virtual platform description;
- virtualize or mediate GIC state;
- load `Image`, initramfs, and device tree; and
- use the same guest-agent and service-export protocol.

## Ostadix guest agent

Build a tiny, auditable `ostadix-agent` placed in the initramfs. It must:

- identify the exact guest package digest;
- negotiate protocol and feature versions;
- prove liveness before service publication;
- enumerate supported exports;
- accept bounded requests through shared queues;
- report device and service health;
- handle cancellation and deadlines;
- acknowledge quiesce and shutdown;
- report a final generation-bound terminal state; and
- never treat a manifest string as authority.

## Shared queue contract

Every descriptor should contain at least:

```text
world_id
domain_id
domain_generation
request_id
operation_code
rights_required
memory_view_capability
input_length
output_limit
deadline
cancellation_generation
checksum or authentication tag
```

No raw host or guest pointer crosses the boundary. Memory is shared only through pinned, bounded view capabilities. Descriptor consumption and completion have explicit linearization points.

## Lifecycle

The foreign KernelWorld moves through:

```text
admitted -> allocated -> booting -> negotiating -> healthy -> serving
                                               |             |
                                               +-> failed ----+
serving -> draining -> quiesced -> stopped -> reclaimed
serving -> failed -> fenced -> reset -> replaced(new generation)
```

Exports become visible only after health. Failure withdraws them before memory or device reassignment. Replacement creates a new domain generation.

## Acceptance gate G

A real Linux kernel boots under O-core on x86_64 and AArch64 virtual hardware, the guest agent becomes healthy, a service is exported through a capability, the guest is killed while requests are in flight, stale completions are rejected, all memory views are revoked, and a replacement generation resumes service without damaging unrelated KernelWorlds.

---

# 13. Workstream H -- Linux driver domains and real physical devices

## Objective

Make Linux a governed hardware-compatibility organ rather than the sovereign host.

Ostadix should support two complementary reuse paths.

## Path H1 -- binary-contained Linux driver domain

A contained Linux kernel owns one or more physical devices and exports services through the guest agent.

Build the complete assignment lifecycle:

1. discover the device and its topology;
2. prove it is assignable or explicitly isolate its group;
3. bind it to an O-core device object;
4. reset it into a known state;
5. create an IOMMU or SMMU domain;
6. grant bounded DMA windows through capabilities;
7. install interrupt routes tied to the domain generation;
8. assign the device to the Linux KernelWorld;
9. wait for the Linux driver to bind and report health;
10. publish a typed service capability;
11. quiesce on withdrawal;
12. revoke interrupts and DMA before memory reuse;
13. reset the device;
14. destroy the old domain; and
15. allow replacement to bind only under a new generation.

First physical proof targets should be selected for resetability and observability rather than glamour:

- a PCIe or virtio-compatible network interface;
- an NVMe controller;
- a USB host controller or USB Ethernet adapter; or
- another device with documented reset and isolation behavior.

The proof must establish that O-core lacks a native driver for the device, Linux controls it, O-core consumes the service, and failure is contained.

## Path H2 -- source-integrated driver domain

Some SBC devices cannot be cleanly assigned to a guest, lack an independent IOMMU group, or rely on SoC integration that makes full-kernel ownership impractical. Build a Linux Driver Environment for those cases.

This path should:

- compile selected Linux drivers and the minimum dependent kernel subsystems into a restricted driver personality;
- provide a versioned compatibility layer for allocation, locking, timers, work queues, DMA, interrupts, firmware loading, and device-tree access;
- run the result in an isolated O-core user domain;
- expose only typed service endpoints;
- pin the Linux source version and generated compatibility surface; and
- fail closed when a driver uses an unsupported kernel facility.

Do not promise universal source transplantation. Create driver-family profiles with explicit coverage.

## Device service interfaces

Define substrate-neutral services such as:

```text
OBlockDevice
ONetworkInterface
OUsbBus
OInputDevice
ODisplaySurface
OAudioEndpoint
OAccelerator
OSensor
OPowerController
```

A service describes operations, rights, data channels, affinity, reset behavior, failure modes, and receipt fields. Linux, a native O-core driver, or another foreign kernel can implement the same service.

## Hardware qualification matrix

Before selecting the eight-node showcase board, record:

- AArch64 exception level and virtualization support;
- GIC version and PSCI behavior;
- SMMU or IOMMU support and stream-ID topology;
- device-tree and bootloader quality;
- serial recovery path;
- Ethernet and USB controller documentation;
- PCIe availability;
- GPU driver openness and userspace requirements;
- reset behavior of candidate devices;
- firmware and binary-blob dependencies; and
- power and thermal observability.

The board is chosen to serve the architecture, not merely because its enclosure is charming.

## Acceptance gate H

O-core boots bare metal, launches a Linux driver domain, assigns a real physical device, consumes a service through a capability, kills the domain under active I/O, proves DMA and interrupts are revoked, resets the device, launches a replacement generation, and preserves unrelated processes and devices. The final eight-node board must pass this gate through either the binary-contained or source-integrated path, with its path stated explicitly.

---

# 14. Workstream I -- Linux ABI and the Debian personality

## Objective

Provide a familiar Linux-compatible user environment above O-core without confusing a Linux userspace personality with a Linux-hosted container.

The personality is a translation and service layer inside Ostadix. Linux may separately exist as a driver KernelWorld, but it is not the kernel serving the Debian processes.

## Process semantics

Freeze these rules early:

- A conventional Linux process has one node-local address space.
- Its threads remain on that node unless a future explicit migration mechanism moves the entire process image.
- `fork`, `clone`, futexes, and anonymous shared mappings are local process semantics.
- Distributed work is expressed as multiple processes, HGraph operations, services, objects, or explicit World APIs.
- A file descriptor may refer to a remote service through a capability-backed proxy.
- The global process namespace maps World identities to node-local process generations.
- Ostadix does not silently turn ordinary pointers or futex words into network operations.

This preserves Linux compatibility where it is meaningful while keeping distribution honest.

## Syscall expansion program

Expand the current exact static corpus in dependency order.

### I1 -- process foundation

- `exit`, `exit_group`;
- `getpid`, `getppid`, credentials, groups;
- `uname` and bounded system information;
- `brk`;
- `arch_prctl` or AArch64 TLS equivalent;
- `set_tid_address`;
- robust-list primitives;
- process and thread creation;
- `execve` and `execveat`;
- `wait4` and wait-id variants; and
- resource limits.

### I2 -- file and path semantics

- `openat` and `openat2` policy;
- `close`, `read`, `write`, vectored I/O;
- seek, stat, access, chmod, chown, links, rename, unlink, mkdir;
- current-directory and root semantics;
- directory iteration;
- file locks;
- truncation and allocation;
- extended attributes where required; and
- POSIX metadata over WorldFS.

### I3 -- virtual memory

- anonymous and file-backed `mmap`;
- `munmap`, `mprotect`, `mremap`;
- copy-on-write fork;
- shared page cache for local or WorldFS-backed files;
- `msync`, advice, and residency queries with documented approximations;
- memory limits and out-of-memory behavior; and
- no transparent remote anonymous memory.

### I4 -- signals and time

- signal disposition and masks;
- delivery frames and return;
- timers and clocks;
- interval timers;
- sleeps and timer file descriptors;
- process-group and job-control signals; and
- terminal-generated signals.

### I5 -- synchronization and eventing

- futex operations required by glibc and common runtimes;
- pipes and socket pairs;
- poll, select, epoll;
- eventfd, signalfd, timerfd;
- inotify over supported filesystem events; and
- bounded asynchronous I/O compatibility.

### I6 -- networking

- sockets, bind, connect, listen, accept;
- send/receive variants;
- socket options;
- IPv4 and IPv6;
- Unix-domain sockets;
- netlink subsets needed by userland;
- DNS and resolver behavior; and
- network namespaces represented as capability-constrained service views.

### I7 -- terminal and device ABI

- pseudo-terminals;
- `ioctl` framework with typed device-family adapters;
- `/dev/null`, `/dev/zero`, random, tty, pts, event devices;
- block and network proxies; and
- no unrestricted pass-through of foreign ioctl numbers without policy.

## Dynamic ELF and glibc support

Extend `elf_loader.oc` to support:

- `PT_INTERP`;
- auxiliary vectors;
- program headers and randomized load bases;
- TLS templates;
- position-independent executables;
- dynamic loader handoff;
- vDSO or a documented syscall fallback;
- stack, environment, and argument conventions;
- architecture-specific relocation and thread-pointer setup; and
- executable and shared-library mapping through package identities.

The first target is not “all Linux binaries.” It is an expanding, measured compatibility envelope whose gaps are machine-readable.

## Debian root filesystem

Create a reproducible pinned Debian root package with:

- immutable base image;
- writable copy-on-write overlay;
- `/proc` personality service;
- `/sys` capability-filtered hardware and World view;
- `/dev` generated from live capabilities;
- `/run` ephemeral state;
- WorldFS mounted at `/world`;
- resolvable user and group databases;
- shell and core utilities;
- package database; and
- signed provenance.

Milestone ladder:

1. run a dynamically linked `hello` against the pinned dynamic loader;
2. run `dash` or another small POSIX shell;
3. run core file and process utilities;
4. run Python or another substantial dynamically linked runtime;
5. run `dpkg-query`;
6. install a local package with `dpkg`;
7. run network resolution and HTTPS through the World network service;
8. run `apt update` against a pinned repository snapshot;
9. install a package with `apt`; and
10. start selected long-running services under an Ostadix-native service manager or a supported subset of systemd semantics.

Full systemd compatibility is a later hardening target because it pulls in cgroups, namespaces, udev, D-Bus, netlink, and extensive `/proc` and `/sys` behavior. It is not required to prove that a Debian environment works, but its dependencies should be tracked rather than ignored.

## Acceptance gate I

On a physical O-core node, `o shell --personality debian` starts a dynamically linked shell from the pinned Debian root. The user can inspect `/world`, create files on the writable overlay, run pipelines, create processes and threads, use networking, install a signed package with `apt`, and invoke a capability-backed remote service. No Linux host kernel serves those processes.

---

# 15. Workstream J -- distributed HGraph execution and recovery

## Objective

Turn the HGraph from a local execution calculus into the Governor's explicit model of computation across the World.

## Four graph layers

Separate four objects that must not be conflated:

```text
LogicalHGraph
    semantic operations, values, effects, authority requirements, failure classes

DeploymentPlan
    concrete node/domain placement, transfers, capabilities, reservations, fallbacks

RuntimeGraph
    live attempts, queues, object versions, checkpoints, heartbeats, observations

RecoveryPlan
    fencing, replay, replacement placement, object restoration, commit decision
```

A membership change may invalidate a deployment without changing the logical computation.

## Project integration

Make the already-declared project operation kinds real:

- `MaterializeProject`;
- `BuildRoute`;
- `RunRoute`;
- `SelectRoute`; and
- `CompareRouteResults`.

Lower project routes, prerequisites, guards, environment requirements, artifacts, cancellation, and equivalence policies into the HGraph rather than executing them through a parallel hidden runtime.

Add distributed operation kinds such as:

```text
AcquireCapability
ReserveResource
BindObject
TransferObject
CreateStream
SpawnAttempt
StartDomain
InvokeService
CheckpointAttempt
RestoreAttempt
FenceAttempt
CommitResult
PublishArtifact
WithdrawResource
```

## Effect and authority completeness

Every remote operation declares:

- value inputs and outputs;
- completion dependencies;
- World, node, domain, object, service, and device effects;
- capability rights;
- capsule affinity;
- data-size estimates;
- checkpoint policy;
- idempotence and determinism classification;
- commit semantics; and
- acceptable failure domains.

Opaque evaluators may remain opaque internally, but their boundary contract cannot be opaque about authority, declared resources, object inputs, outputs, and capsule ownership.

## Placement

The initial placement objective should be explicit and explainable:

```text
minimize
    transfer_bytes * topology_cost
  + estimated_runtime
  + queue_delay
  + checkpoint_cost
  + energy_cost
  + failure_risk
  + authority_crossing_penalty

subject to
    CPU and memory quotas
    architecture and evaluator availability
    device and accelerator requirements
    capability rights
    capsule affinity
    object locality
    replication policy
    trust and attestation policy
    deadline and priority
```

The scheduler should first use a deterministic heuristic that can explain every decision. More sophisticated optimization can follow only after the receipt format exposes its inputs.

## Failure classes

Every operation or route declares one of:

- **ephemeral** -- loss is final;
- **restartable** -- immutable inputs permit replay;
- **checkpointable** -- state can be resumed from a committed checkpoint;
- **replicated** -- multiple attempts may execute, but one may commit;
- **affinity-bound** -- loss of its capsule owner is reported rather than hidden;
- **transactional** -- external effect requires a Governor commit token; or
- **compensatable** -- failure invokes a declared compensating operation.

Exactly-once execution is not assumed. Ostadix provides exactly-one **global commit** where the operation contract permits it.

## Native execution agents

Each O-core node runs a native executor that can:

- materialize a package or project bundle;
- verify all content hashes;
- resolve local personalities and services;
- acquire delegated capabilities;
- create isolated domains;
- stage object inputs;
- run a route or HGraph region;
- stream logs and observations;
- checkpoint;
- publish artifacts; and
- return a generation-bound terminal result.

## Acceptance gate J

A heterogeneous project lowers to one logical HGraph, executes across at least three physical O-core nodes, invokes both native and Linux-personality operations, transfers artifacts explicitly, loses one execution node, restores a checkpoint on another compatible node, rejects the late stale result, and commits one receipt through the Governor quorum.

---

# 16. Workstream K -- distributed objects, storage, and aggregate capacity

## Objective

Make the World's aggregate RAM and storage useful through explicit objects whose locality, replication, and failure behavior are visible.

## Object service

Build a native `objectd` service with:

- content-addressed immutable chunks;
- Merkle manifests for large objects;
- streaming upload and download;
- checksums at every boundary;
- deduplication;
- replica placement;
- quotas and accounting;
- object leases;
- garbage collection tied to committed references;
- snapshots;
- encryption-at-rest policy; and
- repair after node loss.

Metadata is committed through the Governor. Bulk content moves directly between nodes.

## Object kinds and semantics

### Immutable artifacts

Used for source bundles, packages, executable images, datasets, build products, and receipts. Replication is straightforward because content identity is immutable.

### Mutable leased regions

A mutable region has one current owner generation and optional read replicas. Writes require the owner lease or a transactional service. Ownership transfer is an explicit state transition, not cache coherence by accident.

### Streams

Streams provide bounded, backpressured communication. Their endpoints are capabilities. Loss, reconnection, truncation, and replay policies are explicit.

### Sharded collections

A sharded collection records partitioning, placement, and reconstruction metadata. HGraph operations can be placed near shards and can produce new shard sets without assembling the full dataset on one node.

### Checkpoints

A checkpoint contains process or evaluator state only when a versioned adapter can capture it. Otherwise the state remains a capsule and the task cannot claim checkpointability.

## World filesystem storage

Implement an immutable base plus writable distributed overlay for the Debian personality:

- content-addressed lower layers;
- per-World or per-user writable upper layers;
- POSIX metadata service;
- journaled namespace changes;
- snapshot and rollback;
- replica policy for critical metadata and package state; and
- explicit degraded mode when replicas are unavailable.

## Aggregate memory claims

The status interface may report:

```text
Memory capacity: 61.8 GiB aggregate
Locally allocatable now: 7.6 GiB on node-a
Distributed object capacity: 43.2 GiB free
Pinned accelerator memory: 5.4 GiB
```

It must not print `RAM: 64 GiB` without qualification.

## Acceptance gate K

Store an object larger than the memory of any one node as a sharded or streamed World object, execute a computation over its shards without assembling it centrally, remove a replica node, repair the object, and verify the final content and receipt. The Debian overlay survives loss of one storage member according to its declared replication policy.

# 17. Workstream L -- the governed accelerator fabric

## Objective

Expose physically separate GPUs and other accelerators as one schedulable World service without pretending they share one device-local memory.

## OAccelerator contract

Define a substrate-neutral interface containing:

```text
AcceleratorDescriptor
QueueCapability
BufferCapability
ProgramCapability
DispatchDescriptor
FenceCapability
TransferDescriptor
TopologyDescriptor
HealthState
ResetState
```

The descriptor records architecture, supported intermediate representations, memory classes, queue families, limits, synchronization features, driver-domain identity, device generation, and reset behavior.

## Initial implementation path

Use a contained Linux driver domain to own the physical GPU and userspace driver stack. Export a bounded compute service through the Ostadix guest agent.

The first portable API should favor Vulkan compute and SPIR-V because they provide an explicit command, buffer, and synchronization model across vendors. Vendor-specific CUDA, ROCm, Metal translation, or OpenCL adapters can follow as additional personalities.

The service must support:

- device enumeration;
- program or shader upload by content hash;
- local buffer allocation;
- explicit host-to-device, device-to-host, and device-to-device transfer;
- command submission;
- fences and timeouts;
- quotas;
- telemetry;
- domain failure and reset; and
- generation-bound invalidation of queues and buffers.

A GPU buffer is a capsule or a capability to node-local state. It does not become an OValue merely because it has a name.

## Distributed accelerator scheduling

Add HGraph support for:

- data-parallel batches;
- model or pipeline stages with explicit transfer edges;
- tiled rendering;
- replicated inference services;
- collective operations implemented through explicit streams;
- buffer reuse and locality scoring;
- checkpointable host-side state; and
- fallback to CPU or another accelerator when the operation contract permits it.

The scheduler should model:

- program compatibility;
- device-local memory;
- transfer bandwidth;
- queue occupancy;
- topology;
- thermal and power policy;
- failure rate; and
- data residency.

## Acceptance gate L

At least two physical accelerators on different nodes execute one HGraph workload. Their buffers remain explicitly located, work is partitioned by the Governor, one accelerator domain is reset during execution, recoverable work is resubmitted, and the final receipt records every program, buffer transfer, node, domain generation, and committed output.

---

# 18. Workstream M -- security, enrollment, attestation, and least authority

## Objective

Ensure that adding a machine to the World adds governed capability rather than an unauthenticated attack surface.

## Node identity and enrollment

Each node receives or generates a hardware-bound or securely stored identity key. Enrollment is an explicit ceremony:

1. a prospective node presents its public identity and boot measurements;
2. an administrator or existing World policy approves it;
3. the Governor records the identity and permitted roles;
4. the node proves possession through a challenge;
5. a node generation and short-lived session credentials are issued; and
6. resource publication occurs only after health and policy checks.

Support key rotation, revocation, recovery keys, and an offline World-owner root.

## Cryptographic substrate

Use established, auditable primitives through a versioned crypto provider rather than inventing new cryptography. The initial profile should include:

- Ed25519 or another approved signature primitive for identities and packages;
- X25519 or an equivalent key-agreement primitive;
- an authenticated-encryption mode such as ChaCha20-Poly1305;
- HKDF-SHA-256 for key derivation;
- SHA-256 or BLAKE3-class content hashing under an algorithm-tagged format; and
- forward-secure session establishment.

Algorithm identity is included in every signed object so migration is possible.

## Network capability transport

Local capabilities remain unforgeable CSpace entries. Cross-node delegation uses a bounded delegation certificate or ticket referencing:

- issuer identity and generation;
- capability root or object identity;
- exact rights;
- recipient node or domain where appropriate;
- World epoch and revocation generation;
- expiry or use bound;
- nonce; and
- signature or authenticated session binding.

The receiving node creates a local proxy capability only after checking the authoritative Governor state. Rights can only be attenuated.

## Isolation and denial of service

Add quotas for:

- memory and pinned memory;
- capabilities and namespace fids;
- endpoints and queue depth;
- CPU time and priority;
- object storage and transfer bandwidth;
- guest vCPUs and memory;
- DMA windows and interrupts;
- accelerator memory and submissions; and
- logs and receipts.

Every untrusted parser receives fuzzing and bounded-allocation gates. Every service supports cancellation and backpressure.

## Secure boot and attestation

For hardware that supports it, add a measured-boot chain covering:

- bootloader;
- O-core image;
- configuration and policy package;
- Governor service package;
- foreign-kernel packages; and
- critical driver-domain packages.

Attestation is a policy input, not universal truth. A World may admit unattested hobby nodes under a restricted trust class while reserving sensitive capabilities for measured nodes.

## Acceptance gate M

An unauthorized node cannot join, a revoked node cannot reuse an old session, a delegated right cannot be amplified, a stale domain cannot invoke a replacement device, a replayed task result cannot commit, and a compromised low-trust node cannot access objects outside its declared policy. All denials produce structured receipts without leaking secrets.

---

# 19. Workstream N -- observability, command surface, and human usability

## Objective

Make the new unit of computation legible. The World should be understandable without forcing the user to become a distributed-systems operator.

## Command surface

Build a coherent CLI rather than a collection of unrelated binaries:

```text
o world init <name>
o world join <name>
o world status [--watch]
o world topology
o world events
o world doctor
o world snapshot
o world recover

o node enroll
o node status
o node resources
o node drain
o node leave
o node replace

o plan <project> --world <name>
o plan <project> --world <name> --why <operation>
o plan <project> --effects --authority --locality --failure
o run <project> --world <name>
o task inspect <task-id>
o task checkpoint <task-id>
o task cancel <task-id>

o shell --world <name> --personality debian
o mount --world <name> <service> <path>
o cap inspect <cap-id>
o receipt show <receipt-id>
```

`o plan` must provide layered output:

- concise human plan;
- logical HGraph;
- deployment plan;
- effects;
- authority;
- objects and transfers;
- capsule affinity;
- failure policy;
- alternative placements and reasons;
- JSON, DOT, and canonical receipt formats.

## Live topology view

Provide a terminal interface and machine-readable event stream showing:

- Governor leadership and log health;
- nodes and generations;
- links and measured topology;
- resources and ownership;
- services and personalities;
- tasks and attempts;
- object replicas;
- capability delegations at a safe summary level;
- failures, fencing, recovery, and replacement; and
- evidence status.

The mesh should be visible even while the Debian personality remains convenient.

## Installation and node images

Create reproducible outputs:

- x86_64 UEFI boot image;
- AArch64 UEFI/U-Boot boot image;
- board-specific support package;
- recovery image;
- Governor replica package;
- Linux driver KernelWorld package;
- pinned Debian root package; and
- test-lab provisioning manifest.

A node should be enrollable without hand-editing kernel source. Hardware-specific details belong in signed board-support manifests and drivers.

## Developer SDK

Publish versioned interfaces for:

- OValue schemas;
- native O services;
- 9P/WorldFS services;
- capability-aware RPC;
- object and stream APIs;
- HGraph operation adapters;
- Linux personality adapters;
- driver-domain service adapters;
- evidence gate manifests; and
- receipts.

## Acceptance gate N

A new user can boot a prepared physical node, enroll it into an existing World, watch its resources appear, enter the Debian personality, run a distributed project, inspect why each operation was placed, remove the node, and read the recovery receipt without manually editing IP-address lists or service configuration files.

---

# 20. Workstream O -- evidence, formal models, and adversarial testing

## Objective

Scale the repository's existing claim discipline to the full distributed and hardware system.

## Evidence taxonomy

Extend `evidence/gates.toml` or create a compatible World evidence manifest with explicit classes:

```text
hosted_reference
qemu_tcg_x86_64
qemu_tcg_aarch64
qemu_virtualization
hardware_x86_64
hardware_x86_64_iommu
hardware_aarch64
hardware_aarch64_smmu
multinode_virtual
multinode_physical
fault_injection
security_adversarial
performance_characterization
```

A QEMU-TCG result cannot satisfy a hardware virtualization claim. One physical board cannot satisfy a multinode claim. One architecture cannot satisfy a multi-architecture claim.

## Formal specifications

Write executable or model-checked specifications for the highest-risk invariants.

### Governor model

Use TLA+, PlusCal, Alloy, or another suitable formalism to model:

- leader changes;
- node admission and generation;
- capability root publication and revocation;
- resource leases;
- task attempts and exactly-one commitment;
- object ownership transfer;
- island mode; and
- snapshot recovery.

Check safety under message loss, duplication, delay, partition, and process restart.

### Kernel concurrency model

Model or exhaustively test:

- endpoint rendezvous;
- capability transfer and attenuation;
- mapping publication and teardown;
- process and domain generation reuse;
- device assignment and revocation;
- KernelWorld replacement; and
- SMP linearization epochs.

### Namespace model

Check fid generation, mount replacement, stale references, cache invalidation, and private namespace cloning.

## Fault injection

Build deterministic fault controls for:

- packet loss, duplication, corruption, delay, and reordering;
- network partition;
- Governor pause, crash, restart, and stale leader;
- node power loss;
- executor crash before and after result publication;
- object corruption and replica loss;
- guest kernel crash;
- device timeout and reset failure;
- interrupt storm;
- DMA teardown race;
- memory allocation failure;
- storage exhaustion;
- clock skew; and
- stale credential replay.

## Compatibility measurement

The Linux personality should publish a machine-readable compatibility matrix:

- syscall and flag coverage;
- ioctl families;
- `/proc`, `/sys`, and netlink coverage;
- dynamic loader features;
- tested packages;
- known semantic differences; and
- evidence links.

## Performance characterization

Performance evidence is descriptive, not the product definition. Measure:

- local and remote service latency;
- 9P metadata and data throughput;
- object transfer and repair;
- placement overhead;
- Governor commit latency;
- checkpoint and recovery cost;
- Linux personality syscall overhead;
- driver-domain I/O overhead;
- accelerator submission and transfer; and
- scaling under membership changes.

Report topology and locality rather than collapsing everything into one deceptive number.

## Acceptance gate O

The flagship demo has a machine-readable evidence bundle containing build inputs, hardware inventory, topology, gate results, fault-injection traces, receipts, claims, and non-claims. An independent runner can reproduce all virtual gates and verify all physical transcripts and signatures.

# 21. Integration gate ladder

The workstreams proceed in parallel, but the following gates define convergence. A gate is passed only by one reproducible integrated scenario, not by adding together unrelated demonstrations.

| Gate | Integrated result | What it proves | What cannot substitute for it |
|---|---|---|---|
| **G0 -- constitutional baseline** | World contract, crossing kinds, identities, failure classes, consistency model, and claim taxonomy are versioned | the project has one target rather than several homonymous “world” concepts | prose without executable schemas |
| **G1 -- semantic continuity** | project routes lower into a logical HGraph; receipts use shared World identities; `HostWorld` is separated from governed World state | language, project runtime, and kernel vocabulary can converge | a local route runner beside the HGraph |
| **G2 -- AArch64 native compiler** | O-core compiles and boots under AArch64 QEMU with process, IPC, capability, and lifecycle tests | the SBC target is real in the toolchain | cross-compiling a trivial assembly stub |
| **G3 -- multicore O-core** | x86_64 and AArch64 physical targets execute SMP stress gates | the single-CPU proof model has survived concurrency | multiple single-core VMs |
| **G4 -- native World transport** | three physical O-core nodes communicate through authenticated native transport | O-core can join a network without a Linux host | Linux-hosted node daemons |
| **G5 -- replicated authority** | three native Governor replicas preserve one log across leader loss and partition | the World is logically singular without one physical point of failure | one Governor plus a backup process |
| **G6 -- WorldFS** | per-process namespaces mount live resources from multiple physical nodes and survive churn | Plan 9-style composition has become the World interface | a static directory tree or FUSE mount above Linux |
| **G7 -- real KernelWorld** | a pinned Linux kernel boots under O-core and exports a healthy service | foreign-kernel containment is operational | a synthetic guest or syscall emulator |
| **G8 -- real driver service** | Linux controls a physical device and O-core consumes it through a revocable capability | the driver-reuse thesis has crossed into hardware | virtio-only or kernel-internal fake devices |
| **G9 -- native Debian personality** | dynamically linked Debian userland, `dpkg`, networking, and `apt` run under O-core | the familiar operating environment is a true personality | a container or chroot on Linux |
| **G10 -- distributed execution** | logical HGraph placement, objects, checkpoints, and exactly-one commit work across physical nodes | the Governor governs computation, not just names | SSH fan-out or an external batch system |
| **G11 -- accelerator fabric** | at least two accelerators execute one governed workload with explicit buffer locality and recovery | GPUs are resources in the World rather than separately administered devices | independent scripts on each GPU |
| **G12 -- three-node native World** | all preceding core mechanisms coexist on three physical O-core nodes | the architecture has converged into one computer | separate demos on separate branches |
| **G13 -- eight-node World Alpha** | eight SBC-class nodes form one elastic World, provide Debian, driver domains, objects, distributed execution, and visible failure recovery | Ostadix has constructed the intended new unit of computation | any hosted or simulated aggregate |

## Gate dependency structure

The gates are not a simple waterfall. The intended dependency graph is:

```text
G0 -> G1
 |
 +-> G2 -> G3 -----------+
 |                       |
 +-> G4 -> G5 -> G6 -----+----> G12 -> G13
 |                       |
 +-> G7 -> G8 -----------+
 |                       |
 +-> G9 -----------------+
 |                       |
 +-> G10 -> G11 ---------+
```

G7 and G8 can begin on x86_64 while AArch64 matures. G9 can expand in QEMU while physical networking and SMP mature. G10 can use the hosted oracle for differential tests while its native executor is built. None of those hosted or virtual paths substitutes for G12 or G13.

---

# 22. Seventy concrete pull requests

This is a merge-order skeleton, not a prohibition on parallel branches. Each PR should be narrow enough to review, but its “done” condition must be executable.

## Foundation and semantic convergence

### PR 1 -- replace the conservative World roadmap

Add this full-stack program to `docs/`, update `docs/CLAIMS.md`, the multikernel proposal, and the release evidence schema. Define G0 through G13 and mark hosted World as reference-only.

**Repository status:** landed as the version-1 constitution plus
`evidence/world_alpha_gates.toml`. The registry defines all 14 gates and marks
zero passed. Its first schema is definition-only and cannot certify a passage;
this PR does not itself pass G0.

### PR 2 -- shared World identity types

Add the complete identity and generation vocabulary to Rust and `.oc`, with round-trip tests and invalid-generation cases.

**Repository status:** landed as the bounded Mode 27 identity slice. All 20
identity atoms above have shared typed Rust and `.oc` definitions, and the
strict `OWIDENT` v1 identity-only corpus is byte-identical between the Rust
oracle and native O-core under QEMU TCG. Strict decode rejects malformed and
zero-valued records; hierarchical current/reference checks reject stale
generations and same-generation logical mismatches. Serialized capability IDs remain
descriptive non-authority. `OWIDENT` itself remains an identity-only record and
does not become the separate PR 3 protocol codec, a transport, an OValue
envelope, or a receipt codec; it supplies no Governor or consensus and passes
no G0--G13 gate.

### PR 3 -- canonical World wire codec

Implement deterministic encoding, bounded decoding, schema version negotiation, and cross-language byte oracles.

**Repository status:** landed as the bounded Mode 28 `OWPROTO` v1 codec slice.
The Rust oracle and native `.oc` implementation share deterministic big-endian
records with a 16 KiB hard maximum, caller/negotiated record bounds, four fixed
kinds, strict exact-length and reserved-field validation, and canonical nested
`OWIDENT` descriptions. Their fixed 20-record, 1254-byte corpus--two offers, one
canonical v1 selection, one disjoint rejection, and all 16 identity conformance
records--is byte-identical under QEMU TCG. Offline negotiation selects the
highest common schema version and the smaller record limit, or returns one
exact contextual no-overlap rejection.
This is a record codec and pure negotiation function, not a stream or network
transport, live peer handshake, authenticated session, authority channel,
OValue envelope, receipt codec, Governor, consensus implementation, or
Workstream A acceptance. It passes no G0--G13 gate.

### PR 4 -- freeze OValue core and extension envelope

Split portable core values from versioned extensions. Prove canonical hashing and reject authority-bearing values.

**Repository status:** landed as the bounded Mode 29 `OWVALUE` v1 value and
hash oracle. The format is separate and self-framed rather than a new
`OWPROTO` v1 record kind. It has a 4096-byte record maximum, depth-16 and
128-node limits, an explicit portable-value allowlist, strictly ordered record
fields and scalar-key maps, and a root-only inert versioned extension envelope
whose payload must itself be portable. Rust and native `.oc` must emit the same
fixed 19-record, 928-byte corpus--1856 lowercase hex digits with concatenated
SHA-256 `264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc`--and
the same SHA-256 over each complete record; strict
decode/reencode rejects malformed and noncanonical values, while the hosted
projection rejects capabilities, capsules, and effectful values.

Mode 29 is an offline codec and hash oracle. It does not replace the richer
hosted `OValue` or its canonical-CBOR shim wire format, transport values between
domains, resolve descriptive references into authority, dispatch extensions,
implement PR 5 receipts, or satisfy Acceptance gate A. It supplies no Governor,
consensus, WorldFS, or G0--G13 passage, and QEMU TCG is not physical or
hardware-isolation evidence.

### PR 5 -- canonical execution receipt

Unify HGraph, project, live-system, KernelWorld, object, capability, and evidence data into one signed receipt schema.

**Repository status:** landed as the bounded Mode 30 `OWRECEIPT` v1 canonical
receipt and signing-preimage oracle. The separate self-framed format binds a
bounded descriptive subset of World identities and generations, SHA-256 content
references, capability-right descriptions, terminal and commit fields,
evidence-gate identity, and an algorithm-tagged signature envelope. Rust and
native `.oc` converge on a fixed two-record, 3,239-byte corpus (6,478 lowercase
hex digits; SHA-256
`1edd90bf881cd42d08e2031482baae4e7c9a95bd78cfa65f0cbe14147c0a2604`) and
its 1,575-byte current and 1,546-byte stale signing preimages. Hosted Rust
performs Ed25519 sign/verify and tamper/wrong-key rejection
with a pinned, explicitly non-secret conformance key. Native Mode 30 validates
receipt and signature-envelope structure but does not claim a general
freestanding Ed25519 verifier.

This is an offline conformance corpus rather than an integrated execution
receipt. HGraph, project, live-system, KernelWorld, object, capability, O-Git,
and evidence paths do not yet emit or consume it in live operation. Descriptive
capability identities and rights grant no authority, and a valid signature does
not establish signer trust, authorization, current World state, or replay/commit
fencing. Mode 30 supplies no production key lifecycle, transport, Governor,
consensus, WorldFS, typed Alpha attestation, Acceptance gate A, or G0--G13
passage. QEMU TCG is not physical or hardware-isolation evidence.

### PR 6 -- governed `ResourceKey` expansion

Add World, node, domain, process, object, capability, namespace, device, and accelerator resources while retaining `HostWorld` only for ambient hosted effects.

**Repository status (2026-08-02): bounded hosted slice implemented.** The Rust
effect model exposes typed World, Governor, node, domain, process, generic
`GovernedResource` (the `ResourceState` role above), object, descriptive
capability, namespace, task-attempt, artifact-publication, device, and
accelerator keys. Device and accelerator views also touch the canonical generic
resource dependency. All governed source spellings are rejected from
user-authored `reads=`/`writes=` declarations, and ordinary opaque hosted work
continues to use `HostWorld`. `scripts/smoke-world-resource-keys.sh` proves the
bounded hosted vocabulary, underlying identity helpers' caller-pair comparison,
HGraph state chaining, alias-aware grounding partition, source-forgery
rejection, and residual `HostWorld` CLI behavior. Grounding checks only the
bound World epoch/membership.

This status is repository-conformance, not O-core Mode 31, a ResourceKey wire
format, production OIR/project/KernelWorld lowering, live Governor or snapshot
authority, namespace service, device discovery/assignment/driver execution,
PCI/DMA/IOMMU isolation, accelerator control, native/QEMU/hardware evidence,
Acceptance gate A, or passage of G0, G1, or any G0--G13 gate. PR 7 still owns
real project operations in HGraph, and PR 9 still owns the full grounding and
locality views.

### PR 7 -- project operations constructed in HGraph

Make `MaterializeProject`, `BuildRoute`, `RunRoute`, `SelectRoute`, and `CompareRouteResults` appear in real project plans.

### PR 8 -- graph-layer separation

Introduce `LogicalHGraph`, `DeploymentPlan`, `RuntimeGraph`, and `RecoveryPlan` data structures with validation and serialization.

### PR 9 -- grounding and locality planner views

Implement `o plan --grounding --authority --locality --failure --why` and make every reported fact traceable to graph data.

## Compiler and architecture work

### PR 10 -- split target-neutral code generation

Refactor `src/ocore/codegen.rs` into target interface, Machine IR, and x86_64 backend without changing current output.

### PR 11 -- Machine IR legalization and liveness

Represent register classes, calls, atomics, memory order, stack objects, relocations, and control flow. Add verifier passes.

### PR 12 -- deterministic register allocator

Implement linear-scan allocation with spilling, fixed-register constraints, callee-save handling, and comparison against the spill-only oracle.

### PR 13 -- complete indirect calls and atomics

Add function pointers, indirect call validation, architecture-neutral atomics, memory barriers, volatile access, and litmus tests.

### PR 14 -- floating-point and SIMD foundation

Add scalar floating-point and target vector classes sufficient for system libraries and accelerator command preparation.

### PR 15 -- AArch64 ABI and ELF object writer

Emit valid AArch64 relocatable objects, stack frames, calls, branches, constants, and required relocations.

### PR 16 -- AArch64 QEMU boot image

Add AArch64 boot assembly, linker script, PL011 output, generic timer, exception vectors, and a first QEMU smoke gate.

### PR 17 -- architecture-neutral runtime extraction

Move process, IPC, capability, memory-object, package, personality, and KernelWorld logic out of `runtime/x86_64` while preserving x86 gates.

### PR 18 -- AArch64 platform layer

Implement page tables, exception dispatch, GICv3, PSCI, device-tree parsing, and virtio-mmio discovery.

### PR 19 -- physical AArch64 boot

Produce UEFI/U-Boot images and boot O-core on the selected reference board with serial recovery and signed hardware evidence.

## Dynamic kernel and SMP

### PR 20 -- typed kernel allocators

Replace critical fixed tables with quota-accounted page, slab, and object allocators. Add exhaustive allocation-failure gates.

### PR 21 -- per-CPU state and secondary-core startup

Bring up multiple cores on x86_64 and AArch64, but initially hold nonboot cores outside ordinary scheduling.

### PR 22 -- global kernel linearization lock

Run ordinary kernel and user work on all cores under one explicit lock, preserving existing single-owner semantics.

### PR 23 -- per-CPU scheduler and cross-core wakeup

Add run queues, interprocessor interrupts, preemption, and CPU-affinity policy.

### PR 24 -- SMP-safe capabilities and IPC

Stress concurrent transfer, attenuation, endpoint rendezvous, revocation, cancellation, and process teardown.

### PR 25 -- address-space generations and TLB shootdown

Make mapping publication, copy-on-write, unmap, and destruction safe across CPUs.

### PR 26 -- virtualization and IOMMU abstraction

Unify x86 nested paging/IOMMU and AArch64 stage-2/SMMU contracts behind capability-governed interfaces.

## Native transport and Governor

### PR 27 -- virtio-net service

Implement a native packet service under x86_64 and AArch64 QEMU with bounded descriptors and capability-owned buffers.

### PR 28 -- physical bootstrap network driver

Support one reference Ethernet or USB Ethernet controller on physical hardware.

### PR 29 -- native IPv4/IPv6 control stack

Implement addressing, neighbor discovery, UDP, TCP, DHCP/static configuration, DNS, and diagnostics.

### PR 30 -- authenticated multiplexed World transport

Add node authentication, replay protection, streams, flow control, cancellation, reconnect, and resumable object transfer.

### PR 31 -- Governor state-machine specification

Implement the authoritative state machine in Rust and `.oc` without consensus, with model-based differential tests.

### PR 32 -- Raft log and durable term state

Add election, replication, persistence, and committed-state application to the reference implementation.

### PR 33 -- native Governor replicas

Run the same log protocol as O-core user services and prove leader replacement on three QEMU nodes.

### PR 34 -- snapshots and membership reconfiguration

Add log compaction, snapshot install, replica replacement, and crash recovery.

### PR 35 -- leases, fencing, and island mode

Implement resource liveness, node generations, task attempt fencing, capability revocation epochs, and minority restrictions.

### PR 36 -- physical three-replica Governor

Pass G5 on three physical O-core nodes under controlled partition and restart.

## WorldFS and namespaces

### PR 37 -- reusable 9P codec

Promote the bounded Mode 26 codec into a library with adversarial length, mutation, and allocation tests.

### PR 38 -- native 9P server core

Implement attach, walk, open, create, read, write, clunk, remove, stat, and wstat over local IPC and network transport.

### PR 39 -- 9P2000.L compatibility layer

Add Linux-oriented metadata, directory, link, lock, and xattr operations needed by the Debian personality.

### PR 40 -- per-process namespace service

Implement bind, mount, unmount, cloning, inheritance, and namespace policy templates.

### PR 41 -- generation-bound WorldFS

Expose `/world`, bind fids to object generations, publish invalidation events, and reject stale references.

### PR 42 -- WorldFS on physical multinode transport

Pass G6 with resources and services from at least three physical O-core nodes.

## Real Linux KernelWorld and driver domains

### PR 43 -- foreign-kernel package format

Define kernel, initramfs, boot, quota, service, device, health, and provenance fields with strict admission.

### PR 44 -- real x86_64 Linux boot

Replace the synthetic Mode 23 guest with a pinned Linux kernel and initramfs under AMD SVM; add VMX after the interface stabilizes.

### PR 45 -- AArch64 Linux guest boot

Boot the same guest-agent contract at EL1 under O-core at EL2 with stage-2 translation.

### PR 46 -- Ostadix guest agent and shared rings

Implement health negotiation, service discovery, bounded descriptors, cancellation, deadlines, and terminal-state reporting.

### PR 47 -- hostile foreign-kernel lifecycle gates

Crash, hang, replace, and race the guest while proving memory views, queues, and stale completions are revoked.

### PR 48 -- physical-device object and reset state machine

Model discovery, isolation group, DMA windows, interrupts, assignment, quiesce, reset, and generation replacement.

### PR 49 -- first physical Linux-driven device

Pass the complete device lifecycle on a resettable NIC, NVMe, USB controller, or equally bounded device.

### PR 50 -- source-integrated Linux Driver Environment

Create the compatibility surface, one transplanted driver-family profile, and a typed service export for hardware that cannot be cleanly assigned.

## Linux ABI and Debian

### PR 51 -- dynamic ELF, TLS, and `PT_INTERP`

Run a dynamically linked hello-world under both x86_64 and AArch64 O-core Linux personalities.

### PR 52 -- writable VFS and POSIX paths

Add openat-family semantics, metadata, directories, links, rename, locks, and WorldFS-backed files.

### PR 53 -- process creation and copy-on-write

Implement exec, fork/clone, wait, thread-local state, copy-on-write, and process generations.

### PR 54 -- signals, futexes, pipes, epoll, and ptys

Support glibc threading, shell job control, pipelines, event loops, and interactive terminals.

### PR 55 -- sockets and network-facing Linux ABI

Map sockets, DNS, Unix sockets, eventing, and bounded netlink subsets onto Ostadix network services.

### PR 56 -- `/proc`, `/sys`, `/dev`, and ioctl families

Generate capability-filtered views and typed device adapters needed by ordinary userland.

### PR 57 -- reproducible Debian root package

Build immutable base layers, writable overlay, users, groups, resolver state, WorldFS mount, shell, and core utilities.

### PR 58 -- `dpkg` and local package installation

Install, remove, query, and verify signed local Debian packages inside the native personality.

### PR 59 -- `apt` through World networking

Update from a pinned repository snapshot and install a package through the O-core network and storage services.

## Distributed execution, objects, accelerators, and convergence

### PR 60 -- native content-addressed object service

Add chunking, Merkle manifests, replication, repair, quotas, garbage collection, and receipts.

### PR 61 -- native distributed HGraph executor

Place HGraph regions on O-core nodes, acquire capabilities, stage objects, run attempts, and publish results.

### PR 62 -- checkpoint and recovery adapters

Support native processes, selected evaluators, and route-level checkpoints while retaining capsule classification for unsupported state.

### PR 63 -- exactly-one global commit

Fence duplicate attempts, external effects, and late results through the Governor.

### PR 64 -- distributed Debian storage overlay

Replicate package and user state, snapshot it, and recover after a storage-node loss.

### PR 65 -- OAccelerator service contract

Implement programs, queues, buffers, dispatch, fences, telemetry, reset, and capability rights.

### PR 66 -- first Linux-driven GPU compute service

Run Vulkan compute or SPIR-V through a Linux driver domain and consume it from an O-core HGraph.

### PR 67 -- multi-accelerator placement and recovery

Partition a workload across at least two nodes, reset one accelerator domain, and recover permitted work.

### PR 68 -- three-node native convergence gate

Pass G12 with replicated Governor, WorldFS, Debian, a real Linux driver service, objects, and distributed HGraph execution in one scenario.

### PR 69 -- eight-node provisioning and topology

Build reproducible images and enrollment manifests for the final SBC fabric.

### PR 70 -- Ostadix World Alpha gate

Pass G13, publish the full signed evidence bundle, and cut the first release that may call itself the machine constructor.

---

# 23. The flagship eight-node demonstration

The flagship must show the ontology changing, not merely a benchmark number increasing.

## Physical layout

```text
Eight SBC-class AArch64 nodes
    three Governor replicas
    five additional execution/storage/accelerator members
    Ethernet or another explicit fabric
    O-core bare metal on every counted node
    Linux KernelWorlds only as contained driver or compatibility domains
```

The nodes need not be identical. Heterogeneity strengthens the claim if architecture, memory, device, and capability differences are represented honestly.

## Boot and formation

Each node boots O-core, verifies its packages, starts native services, authenticates to the World, and requests admission.

The operator sees:

```text
$ o world status --watch

World: desk
Epoch: 184
Governor: 3 replicas, quorum healthy
Members: 8 healthy
Architectures: aarch64
CPU: 32 allocatable cores
Memory: 61.8 GiB aggregate, node-local
Object capacity: 412 GiB replicated
Accelerators: 3 available, 1 degraded
Driver worlds: 8 healthy
Debian personality: ready
```

The status output explicitly labels aggregate capacity and locality.

## Familiar environment

```text
$ o shell --world desk --personality debian
root@desk:/# uname -a
root@desk:/# ls /world/members
root@desk:/# apt update
root@desk:/# apt install <pinned-demo-package>
```

The shell runs under the O-core Linux personality. Networking and at least one physical device are provided by contained Linux driver worlds.

## Distributed workload

Run a project whose HGraph includes:

- a sharded input object larger than one node's chosen working-set budget;
- parallel preprocessing on multiple nodes;
- at least two accelerator operations;
- a checkpointable long-running stage;
- a route implemented by two personalities under `verify_equivalent`; and
- one final transactional artifact publication.

Before execution:

```text
$ o plan demo.O --world desk --authority --locality --failure
```

The plan explains placement, transfers, capabilities, capsules, checkpoints, and fallbacks.

## Failure sequence

1. Start the workload.
2. Physically disconnect one ordinary worker node.
3. Observe its lease expire and its resources leave `/world`.
4. Verify that unrelated work continues.
5. Restore checkpointable work on a compatible node.
6. Verify that an affinity-bound operation reports loss rather than silently migrating.
7. Reconnect the old node and confirm that it receives a new generation.
8. Submit a delayed result from the old attempt and observe fencing.
9. Kill one Governor replica and verify quorum continuation.
10. Add a ninth previously unenrolled node, approve it, and watch new resources appear.
11. Drain a node intentionally and verify graceful task relocation and service withdrawal.

Expected event narrative:

```text
node-5 generation 12: lease expired
resources withdrawn: cpu/4, memory/7.6GiB, accelerator/gpu0
attempt 7 fenced
checkpoint 44 selected
replacement attempt 8 placed on node-2 generation 31
affinity-bound camera stream reported LOST_ORIGIN
late completion from attempt 7 rejected
node-5 reenrolled as generation 13
governor replica g2 unavailable; quorum remains 2/3
node-9 generation 1 admitted; resources published
```

## Final evidence

The final receipt records:

- exact hardware and firmware inventory;
- O-core and package digests;
- World epoch and Governor terms;
- membership changes;
- every placement and transfer;
- delegated rights;
- Linux driver-domain generations;
- accelerator programs and buffers;
- checkpoints and recovery;
- stale-result denial;
- final artifact hashes; and
- claims and non-claims.

The headline is not “eight boards beat a workstation.” It is:

> Eight independent machines entered one governed computational World, the World gained and lost organs while running, and the user continued to operate one coherent environment without manually programming the distribution.

---

# 24. Hardware development program

The final system is hardware-facing, so hardware cannot remain an undifferentiated future concern. Use a four-tier lab.

## Tier 1 -- deterministic virtual platforms

Use QEMU x86_64 and AArch64 for:

- compiler and ELF gates;
- boot and exception gates;
- virtio networking and block devices;
- SMP stress;
- Governor partitions;
- WorldFS protocol mutation;
- Linux guest boot; and
- deterministic fault injection.

Virtual platforms maximize observability. They do not establish physical-device or timing claims.

## Tier 2 -- x86_64 virtualization and IOMMU reference machine

Use a machine with well-understood AMD-V or Intel VT-x, IOMMU, PCIe, and resettable devices to mature:

- nested paging;
- physical device assignment;
- DMA teardown;
- interrupt remapping;
- reset and replacement;
- Linux driver-domain service export; and
- hostile failure tests.

This platform attacks the hardest driver-domain unknown with the richest debugging environment. It is not the final form factor.

## Tier 3 -- one AArch64 reference SBC

Select one board using the qualification matrix from Workstream H. Prove:

- repeatable boot and recovery;
- multicore startup;
- physical memory and device-tree discovery;
- native control networking;
- EL2 guest execution;
- SMMU or an explicit alternative isolation strategy;
- one real driver service;
- thermal and power observability; and
- local Debian personality execution.

Do not purchase eight showcase boards until one board passes this gate. This is not conservatism about the goal. It is protection against multiplying the wrong hardware assumption eightfold.

## Tier 4 -- eight-node fabric

Only after the reference board passes its native gates should the eight-node lab be assembled. The lab needs:

- managed power control or remotely controllable outlets;
- switch ports capable of per-link fault injection;
- serial consoles or a serial concentrator;
- reproducible boot media;
- one independent management machine;
- measured link topology;
- at least one removable worker device;
- at least one accelerator-capable member; and
- sensors for temperature, power, and link state.

The lab itself becomes an evidence instrument.

## Board-support package contract

A board-support package should contain:

```text
board identity and revision
supported boot path
firmware and bootloader requirements
device-tree digest and overlays
CPU topology
memory map
interrupt controller
timers
serial console
IOMMU or SMMU topology
network bootstrap path
assignable devices
native drivers
foreign-driver profiles
known reset limitations
power and thermal sensors
claims and non-claims
```

The kernel must not accumulate board-specific conditionals without a corresponding signed support package and hardware gate.

---

# 25. Risk register with architectural responses

Ambition is useful only when the risks are named in the same resolution as the goals.

| Risk | Why it is dangerous | Required response |
|---|---|---|
| **Compiler refactor destabilizes the existing kernel** | code generation is beneath every native gate | preserve x86 output through differential tests, keep spill-only backend as oracle, merge target-neutral layers incrementally |
| **AArch64 becomes a second ad hoc backend** | architecture drift would duplicate the kernel | force Machine IR and architecture-neutral runtime before broad AArch64 feature growth |
| **SMP breaks generation and capability invariants** | stale authority bugs become nondeterministic | begin with one explicit linearization lock, model commit points, stress before lock partitioning |
| **Target SBC lacks usable EL2 or SMMU behavior** | binary-contained driver domains may be impossible or unsafe | qualify hardware before scale-out, maintain source-integrated driver path, choose a different board when the architecture requires it |
| **Physical devices cannot be reset or isolated** | failed driver worlds could retain DMA or poison replacements | select resettable reference devices, expose isolation groups, deny assignment when teardown cannot be proved |
| **Linux guest boots but useful drivers remain inaccessible** | “Linux as hardware organ” would stay ceremonial | require a physical-device gate and a typed service consumed by an unrelated O-core process |
| **Linux ABI expansion becomes an endless syscall list** | broad compatibility can swallow the project | drive coverage through dependency slices tied to concrete Debian packages, publish a machine-readable compatibility envelope |
| **WorldFS becomes a slow universal data tunnel** | control-plane elegance could strangle bulk workloads | negotiate separate streams and object channels while retaining 9P naming and authority |
| **Consensus infects every fast path** | a global log on each operation would destroy locality | commit identity, authority, metadata, and terminal decisions; keep ordinary local execution and bulk data off the log |
| **Minority partitions create split-brain effects** | two halves could both believe they own authority | enforce island mode and fencing; local-only work must be labeled and cannot globally commit |
| **Checkpoint claims exceed actual state capture** | hidden actor or device state could make replay false | unsupported state remains a capsule; checkpointability is adapter-specific and tested |
| **GPU APIs expose raw foreign handles** | driver-domain affinity and stale generations could escape | export OAccelerator capabilities and explicit buffers, never vendor handles as portable OValues |
| **OValue becomes a universal coupling sink** | every subsystem could become dependent on every variant | freeze a small core, use versioned extension envelopes, keep authority and capsules out of value serialization |
| **Repository complexity overwhelms integration** | many dissertation-sized branches can diverge | organize by integration gates, require shared schemas, and stop counting isolated feature code as progress |
| **Security is postponed until after the demo** | networked capabilities would be unsafe by construction | enrollment, authentication, replay protection, quotas, and revocation are part of the first native transport and Governor gates |
| **A compelling demo encourages inflated claims** | attention can outrun evidence | preserve claim/non-claim manifests and publish the full topology, limitations, and evidence bundle with the demo |

---

# 26. Immediate implementation order

The first work should attack the architectural seams and the longest native poles simultaneously. The next twelve concrete moves are:

## Move 1 -- land the constitutional document

Replace the prior World v0 roadmap with this program. Add G0 through G13 to the evidence vocabulary. Mark the hosted World as a reference implementation and prohibit it from satisfying native release gates.

**Repository status:** complete as a constitutional/schema change. The hosted
profile remains reference material; only its separately bounded lifecycle
oracle and partial libraries are currently executable. It carries no
qualifying gate credit, and no G0--G13 status was promoted.

## Move 2 -- create one shared World identity module

Implement Rust and `.oc` definitions for World, Governor, node, domain, resource, object, task, attempt, capability, and receipt identities. Add byte-level cross-language tests.

**Repository status:** complete at the PR 2 boundary through the shared
20-atom Rust/`.oc` vocabulary and bounded `OWIDENT` v1 QEMU byte oracle.
Capability IDs in the serialized corpus remain descriptive, never authority.
The general PR 3 wire/transport/negotiation/receipt surface, Governor,
consensus, and all G0--G13 qualification remain outside this move.

## Move 3 -- unify `HostWorld@N` and KernelWorld semantics without renaming away opacity

Add precise governed `ResourceKey` variants. Bind native execution to real KernelWorld and World generations. Keep unknown hosted effects under `HostWorld` so the planner can measure the remaining gap.

## Move 4 -- make project HGraphs real

Construct the currently declared project operations from `ProjectBundle` and route policy. Add logical HGraph output for `o plan <project>`.

## Move 5 -- define `DeploymentPlan` and `RecoveryPlan`

Before implementing network execution, freeze how logical operations become placements, transfers, delegated capabilities, capsules, checkpoints, and commit rules.

## Move 6 -- split `src/ocore/codegen.rs`

Create the target interface and Machine IR while proving unchanged x86_64 output. This opens the AArch64 path without immediately rewriting the kernel.

## Move 7 -- implement the AArch64 object and boot minimum

Emit one freestanding AArch64 object, link it, print through PL011 under QEMU, and add the first AArch64 evidence gate.

## Move 8 -- write the Governor state-machine model now

Specify membership, generations, resources, capability roots, tasks, attempts, object ownership, and island mode before network code creates accidental semantics.

## Move 9 -- promote the 9P codec out of the bounded corpus

Preserve the existing wire oracle, move the codec into a reusable library, and fuzz it. WorldFS work can then proceed in parallel with networking.

## Move 10 -- define the real Linux KernelWorld package and guest-agent protocol

Freeze the image, initramfs, health, shared-queue, service-export, and teardown contracts before choosing the first Linux image.

## Move 11 -- create the hardware qualification worksheet

Evaluate candidate AArch64 boards for EL2, GIC, PSCI, SMMU, boot path, Ethernet, PCIe, GPU, reset, and documentation. Select one reference board based on the kernel architecture.

## Move 12 -- establish a convergence CI matrix

Add jobs or lab scripts for:

```text
hosted semantic oracle
x86_64 compiler and current QEMU gates
AArch64 compiler gate
AArch64 QEMU boot gate
Governor model checker
9P mutation tests
foreign-kernel package verifier
three-node network simulation
```

These twelve moves begin the hard paths immediately while preserving the verified ground already present.

---

# 27. Work to stop expanding until it serves the World

An ambitious roadmap still needs exclusion. The following work should not consume primary engineering attention unless it directly satisfies one of G0 through G13:

- additional hosted evaluator languages;
- unrelated syntax additions;
- more hardcoded O-Git demo fields;
- a second hosted orchestration framework;
- additional foreign personalities before Linux reaches a real device and Debian gate;
- generalized transparent distributed shared memory;
- arbitrary process migration before checkpoint adapters exist;
- rich graphical administration before the terminal event model is complete;
- performance tuning that does not expose placement and topology; and
- support for poorly documented boards chosen only for price or novelty.

This is not a reduction in ambition. It concentrates ambition on the parts that change what a computer can be.

---

# 28. Alpha non-claims

Even the ambitious Alpha must remain precise. Passing G13 would not yet prove:

- uniform coherent RAM across nodes;
- remote pointer transparency;
- arbitrary unmodified Linux kernel modules inside the O-core kernel;
- universal hardware support;
- arbitrary Linux binary compatibility;
- transparent migration of every Linux process;
- perfectly shared multi-GPU memory;
- production-grade Byzantine-fault tolerance;
- safety against malicious physical hardware;
- every systemd service or desktop environment;
- Windows, XNU, Android, or Plan 9 binary compatibility; or
- that eight inexpensive SBCs outperform a tightly integrated workstation on all workloads.

It would prove something more structurally important:

> O-core can constitute multiple physical machines, foreign kernels, devices, namespaces, user environments, data objects, and execution attempts as one capability-governed World whose membership may change without destroying its identity.

---

# 29. Beyond Alpha: Ostadix World 1.0

After the eight-node Alpha, the road to 1.0 is not “add more nodes.” It is hardening the machine as an ecosystem.

World 1.0 should add:

- rolling upgrade of Governor replicas and native services;
- stable wire-compatibility policy;
- multi-user identity, groups, quotas, and delegation;
- secure and measured boot profiles;
- encrypted object storage and backup;
- automatic replica repair and capacity rebalancing;
- service-level objectives and admission control;
- broader Debian package compatibility;
- fuller cgroup, namespace, seccomp, udev, D-Bus, and systemd behavior;
- explicit whole-process checkpoint and migration for supported process classes;
- high-speed transports such as 10/25 GbE or RDMA where hardware permits;
- topology-aware collective operations;
- additional accelerator personalities;
- a native Plan 9 user environment above the same WorldFS;
- Android/Binder, NT, and XNU personalities only after the Linux crossing machinery is reusable rather than special-cased;
- source-integrated driver profiles for common SBC device families;
- reproducible board-support kits; and
- a public conformance suite that third-party nodes and personalities can run.

The long-term ecosystem goal is that a new node, driver world, evaluator, or operating-system personality can join through published contracts rather than through privileged edits to the center of Ostadix.

---

# 30. Final definition of success

The project has crossed from research architecture into a new computing capability when all of the following are simultaneously true:

- the user can assemble a World from physically separate machines;
- O-core, not Linux, is the constitutional kernel on the counted nodes;
- Linux remains available as a governed driver and compatibility organ;
- resources are named globally but remain honest about locality;
- authority crosses only through attenuated capabilities;
- affinity remains explicit as capsules;
- a familiar Debian environment is available without hiding the World;
- computation can be planned, placed, recovered, and explained through the HGraph;
- nodes can appear and disappear without stale authority corrupting the World;
- the Governor survives a replica loss;
- data and accelerators are usable as explicit distributed resources;
- every result carries a receipt; and
- every public claim is bounded by executable evidence.

At that point, Ostadix is not a cluster manager wearing an operating-system costume. It is not a Linux distribution spread over several IP addresses. It is not Plan 9 with a different shell. It is not a hypervisor with a global dashboard.

It is the operational form of the original proposition:

> The identity of a computer lies in the governed structure among its computational resources, not in the enclosure that happens to contain them.

The eight-board machine is not the metaphor for that proposition. It is the experiment that makes the proposition material.

---

# Source and verification note

This constitution was reconciled against canonical repository commit
`7b9e91c` before its first in-repository revision. That baseline contains the
HGraph, project runtime, effects, capability bridge, hosted lifecycle,
KernelWorld identity and lifecycle code, x86_64 O-core runtime, bounded Linux
personality, bounded Linux/9P composition, QEMU gate scripts, and the bounded
`evidence/gates.toml` manifest. This revision adds the separate World Alpha
registry.

The validator and repository suites run separately from this prose. A future
gate is not implemented merely because it is defined here or in the registry.
The current 21 portable QEMU gates and one supplemental hardware gate retain
only the bounded claims in [`CLAIMS.md`](CLAIMS.md) and
[`evidence/gates.toml`](../evidence/gates.toml). Their results do not satisfy a
G0--G13 gate. Schema v1 admits no evidence records; only a future versioned,
typed-attestation schema may bind a current or new result to an exact gate.
