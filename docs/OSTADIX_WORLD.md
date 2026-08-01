# Ostadix World v0 Contract

Status: normative product and architecture contract. This document fixes the
meaning of Ostadix World v0 and the claim boundary for its first hosted demo.
It is not evidence that the distributed Governor, membership protocol,
resource registry, namespace, route dispatcher, or placement layer exists.
Those claims require their own executable acceptance gates.

## Product definition

**Ostadix World is an elastic, capability-governed computational fabric.**

A node joins a World and contributes typed, named resources. If the node leaves
or fails, those resources are withdrawn. Computation is placed according to
resource availability, authority, locality, and failure policy. A Linux/POSIX
personality may provide a familiar user environment, but it is a projection of
the World. It does not make remote resources physically local.

The governing statement is:

> A computer is not a box. A computer is a governed membership of
> computational resources.

Memory is initially reported and scheduled as aggregate capacity, but an
ordinary process remains inside one node's local address space. Cross-node
applications communicate explicitly through OValues, capabilities, streams,
content-addressed artifacts, or later distributed operators.

### Explicit v0 non-goals

Ostadix World v0 does not provide or claim:

- coherent shared RAM across nodes;
- transparent remote pointers, distributed threads, or remote instructions;
- one physically fused GPU, accelerator, or memory bus;
- arbitrary Linux binary compatibility or a distributed POSIX process tree;
- a general Plan 9 namespace, mount environment, or filesystem implementation;
- physical-device passthrough or physical-device reuse across nodes;
- automatic process migration;
- transparent relocation of affinity-bound work; or
- aggregate-performance or speedup results.

## Vocabulary and identity

Names and inventory are descriptive. They do not grant authority. Every live
identity is interpreted with its owning generation or epoch; equality of an
integer or string never revives an older object.

- **World**: one governed computational fabric under a named `WorldId` and a
  persistent, monotonically increasing `WorldEpoch`. A snapshot records one
  exact epoch.
- **Governor**: the sole v0 authority that admits nodes, advances the World
  epoch, grants leases, publishes resources, places tasks, fences attempts, and
  commits globally authoritative results.
- **node**: a hosted or native execution substrate identified by `NodeId` and
  `NodeGeneration`. Rejoining the same named node creates a fresh generation.
- **domain**: an isolated execution world on one node, identified independently
  by `DomainId` and `DomainGeneration`. A KernelWorld may be bound into this
  vocabulary, but its serialized identity is not authority.
- **resource**: a typed, named, generation-bound contribution such as CPU
  execution slots, node-local memory budget, accelerator endpoints, artifact
  capacity, or a service. A `ResourceId` does not by itself authorize use.
- **lease**: time-bounded authority issued by the Governor. A node lease keeps
  one node generation admitted; a task lease keeps one attempt eligible to
  produce a globally committable result.
- **task**: one scheduled unit of coarse work, identified by `TaskId`. Each run
  has an `AttemptGeneration`; a late result from an older attempt is fenced.
- **artifact**: immutable content-addressed input or output. Its content digest
  is data identity, not authority to read, publish, or execute it.
- **capability**: live authority issued through an authenticated,
  generation-bound broker with explicit rights. Transfer preserves or reduces
  rights and never treats names or metadata as grants.
- **capsule**: a live or native object whose meaning is bound to an origin node,
  domain, process, generation, lifetime, and rehydration policy. It remains
  nonportable until a versioned, tested adapter proves otherwise.
- **WorldSnapshot**: an inert observation of membership, resources, leases, and
  load at one World epoch. It is input to placement, not live authority.
- **DeploymentPlan**: a physical realization derived from a logical computation
  and a snapshot:

  ```text
  DeploymentPlan = place(LogicalHGraph, WorldSnapshot, PlacementPolicy)
  ```

  Membership changes may invalidate or recompute the deployment without
  changing the logical HGraph's semantic identity.

## The three crossing categories

Every cross-node or cross-personality object belongs to exactly one category.
No transport silently changes categories.

1. **OValue -- data.** Canonically serializable, bounded, copyable, and
   replayable when its runtime-boundary classification permits it.
2. **Capability -- authority.** Transferable only through an authenticated,
   generation-bound broker and only with equal or reduced rights.
3. **Capsule -- affinity.** Bound to an origin node, domain, process,
   generation, lifetime, and rehydration policy. It is nonportable until a
   tested adapter proves otherwise.

The type-system rule is:

> Portability may be epistemically incomplete, but the runtime treats unproved
> portability as nonportability.

A capsule may be promoted to an OValue or capability only through a versioned,
tested transport morphism whose output category is explicit.

## Four-plane architecture

The World separates discovery, authority, execution, and bulk transfer:

1. **Namespace/control plane.** A generation-bound 9P-style `/world` tree for
   discovery, inspection, mounting, and bounded control operations.
2. **Authority plane.** Ostadix capabilities issued by the Governor and private
   brokers. Namespace lookup and inventory strings never grant authority.
3. **Execution plane.** Governor requests that place and execute whole project
   routes, services, or HGraph regions. v0 does not schedule individual machine
   instructions, threads, or pointer dereferences across nodes.
4. **Bulk-data plane.** Content-addressed artifact transfer or separately
   negotiated bounded streams. High-rate data is not forced through ordinary
   9P messages.

9P is therefore a namespace and control protocol, not a claim that every
artifact, tensor, remote page, or device stream is a 9P file operation.

## Membership and partition policy

World v0 starts with hosted Linux node daemons and one authoritative Governor.
The protocol and identity rules remain substrate-neutral so a later O-core node
can implement the same boundary without changing the user-visible World.

The Governor persists a monotonic World epoch and issues renewable node leases.
A heartbeat may renew only the exact admitted node generation. When a lease
expires:

- that generation's resources and namespace entries are withdrawn;
- no new global authority may be minted through it;
- stale generation-bound references and capabilities are rejected;
- work on unaffected nodes continues;
- local work may finish, but it cannot commit globally unless its task lease and
  attempt generation remain current; and
- rejoining creates a fresh node generation.

World v0 does not implement consensus. During a partition, the side that cannot
reach the authoritative Governor may continue explicitly local work, but it may
not mutate the shared namespace, renew global leases, publish global resources,
or commit globally authoritative results. A later replicated Governor must
preserve these fencing rules.

## State machines

### Node lifecycle

```text
Discovered
    -> Admitted(generation N, lease)
    -> Healthy
    -> Draining
    -> Left

Healthy
    -> Suspect
    -> LeaseExpired
    -> Lost
    -> Admitted(generation N+1, new lease)
```

No object from generation N becomes valid in generation N+1 because a name,
slot, or integer was reused.

### Task lifecycle

```text
Submitted
    -> Planned
    -> Leased(attempt N, node generation G)
    -> Running
    -> ResultPendingCommit
    -> Committed

Running -> Lost -> Rescheduled(attempt N+1)
Running -> Cancelled
Running -> Failed
ResultPendingCommit -> FencedLateResult
```

The Governor is the only v0 authority that commits a transactional result. Each
task declares one failure class: `ephemeral`, `restartable`, `replicated`,
`affinity-bound`, or `transactional`. An affinity-bound task is reported
unavailable when its origin is lost; it is not silently moved.

## First placement policy

The initial policy is deliberately deterministic and inspectable. For each
route it:

1. filters to healthy nodes with current leases;
2. requires every declared service and capability kind;
3. requires sufficient CPU slots, node-local memory budget, and accelerator
   endpoints;
4. enforces capsule and device affinity;
5. prefers nodes that already hold the input artifacts;
6. prefers lower declared load; and
7. uses node identity as the final deterministic tie-breaker.

Machine learning, global optimization, transparent migration, and performance
claims are outside the first scheduler.

## First hosted demo claim boundary

The first complete hosted World demo may claim only what its process-level gate
observes:

- two or more hosted node daemons join one Governor under distinct,
  generation-bound leases;
- admitted nodes publish bounded typed resource quotas;
- one existing project route is placed on and executed by a non-Governor node;
- inputs and outputs are content-addressed and the result carries a World,
  node, attempt, route, authority, and artifact receipt;
- lease expiry withdraws one node's resources and stale authority;
- unrelated nodes continue making progress;
- a restartable attempt is fenced and rescheduled once while an affinity-bound
  attempt is reported unavailable;
- a late result cannot overwrite the current attempt's committed result; and
- rejoining the same node creates a fresh generation without reviving old
  capabilities, fids, or task leases.

That demo does **not** prove bare-metal operation, an O-core node, replicated
Governor consensus, general 9P, Linux or Plan 9 boot, a distributed Linux ABI,
coherent memory, transparent migration, KVM/SVM hardware isolation, PCI or
physical-device assignment, DMA/IOMMU isolation, hardware reset, or aggregate
performance.

## Current repository boundary

The repository already contains reusable but separate organs: hosted HGraph and
effect execution, project routes, the hosted Live-World package/service oracle,
the private capability broker, KernelWorld contract and bounded Modes 20--23,
Mode 25's exact static Linux ELF corpus, and Mode 26's exact bounded 9P2000
client/server corpus. Their individual claims remain those in
[`CLAIMS.md`](CLAIMS.md) and [`evidence/gates.toml`](../evidence/gates.toml).

The partial PR1 foundation provides validated, nonzero,
generation-qualified shared identities, explicit stale-reference comparison,
descriptive KernelWorld-to-domain binding that requires caller-supplied
registry-allocated `DomainIdentity` placement context while keeping the
provider lifecycle generation separate, and precise governed `ResourceKey`
variants that remain separate from ambient `HostWorld`. Its inspection surface
is:

```bash
olangc file.O --target ir --grounding \
  --world-id desk --world-epoch 4
```

The report exposes logical OValue edges, requested backend capability rights
and ambient fallback, unresolved capsule affinity, any governed resources
already present in the graph, and residual host effects. Serialized metadata
never supplies granted rights. Current-snapshot enforcement is absent.
`--world-id` and `--world-epoch` descriptively bind the report to
caller-supplied identity; the command does not consult a live World snapshot or
enforce current-epoch freshness. No production lowering currently emits
governed `ResourceKey` effects. Ordinary hosted `.O` plans therefore retain
their conservative `HostWorld` effects. This is identity/effect/grounding
groundwork, not completion of PR1's execution-generation criterion, project
placement, node admission, or distributed execution.

Those parts do not compose into a distributed World today. In particular, the
repository has no completed distributed Governor, node-membership daemon,
networked resource registry, generation-bound `/world` tree, remote route
dispatcher, failure/rescheduling loop, or live placement engine. The shared
governed identity/effect vocabulary is the integration foundation, not evidence
for the later multi-node demo.
