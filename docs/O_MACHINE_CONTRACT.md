# O-Machine EL2 and O-core Resource Contract

**Status:** normative design contract for the machine-facing boundary required
by G7 and G8. The current G2 AArch64 gate proves only that O-core can keep EL2
resident, enter host EL1, and complete one checked host-EL1-to-EL2 HVC return.
It does not implement the resource objects or revocation protocol specified
here.

This document refines the EL2 authority boundary in
[`OSTADIX_WORLD.md`](OSTADIX_WORLD.md). The bounded KernelWorld implementation
status remains in [`KERNEL_WORLD_CONTRACT.md`](KERNEL_WORLD_CONTRACT.md).
Its executable design vocabulary is
[`evidence/o_machine_contract_v1.toml`](../evidence/o_machine_contract_v1.toml);
that schema is not implementation evidence.

## Trust and policy boundary

O-core at host EL1 is inside the trusted computing base for **authority**. It
decides which World receives a resource, which semantic rights are granted,
which dependencies cascade, and which guest-facing failure policy applies.
O-Machine at EL2 is inside the trusted computing base for **machine memory
safety**. It does not reproduce O-core's policy graph. It owns only the
irreducible facts needed to reject stale machine state and cross-World frame
access:

```text
machine_incarnation
world_generation[world_slot]
resource_generation[resource_slot]
page_owner[physical_frame]
```

Both generation arrays use checked, nonzero 64-bit values. A generation never
wraps or repeats for the same slot; exhaustion fails closed and permanently
retires that slot. EL2 rejects zero, non-monotonic, wrapped, or reused values
rather than allowing an old handle or completion tombstone to alias a new
incarnation.

Tracking a World generation does not let EL2 create or admit a World. O-core
requests the lifecycle transition as authority policy; EL2 only records the
next checked value needed to prevent stale machine-state aliasing. Before
requesting an advance, O-core's composite World retirement must contain every
old-World class-specific `HostResourceAck`, including terminal broker work and
its journal record. EL2 separately refuses the mechanical advance until every
vCPU, guest/host mapping, pin, DMA window, and interrupt effect is
machine-acknowledged and no page remains `Owned` by the old World generation.
Pages may be `Quarantined` at this point, but they cannot be reassigned before
scrubbing. A logical World-generation bump over live hardware or live broker
work is forbidden; EL2 still does not interpret the broker graph.

EL2 must reject a mapping when the target World does not own the physical
frame, even if host EL1 presents an otherwise well-formed request. It must also
reject an obsolete generation after a resource slot is retired. This limits an
EL1 bug from mapping World B's frame into World A. It does not make EL1
untrusted for authority: a compromised EL1 can still grant any policy-level
right that O-core is entitled to grant.

This memory-safety claim also forbids an unrestricted host-EL1 physical/direct
map of World-owned frames. Guest mappings and temporary O-core broker views
must both traverse an EL2-controlled translation context that checks
`machine_incarnation`, owner, World generation, resource generation, bounds,
and access rights. A virtio/9P buffer pin is a scoped, generation-bound broker
view, not permission for EL1 to address arbitrary RAM. EL2 includes every such
host view and pin in teardown and acknowledgment. If a target architecture
cannot interpose on host-EL1 access this way, it cannot claim the stronger G7
cross-World memory-safety boundary against EL1 bugs.

The G7 boundary also assumes that no unfenced physical bus master can DMA into
World-owned frames. Its virtio demonstration uses emulated/brokered devices;
any real DMA-capable device requires the G8 class contract plus an enforced
IOMMU/SMMU domain. If host EL1 can program an unmediated bus master, neither
`page_owner` nor CPU stage-2 checks establish cross-World memory safety.

The enforcement state itself is outside host authority. EL2 code/data,
stage-2 roots, machine-incarnation and generation tables, `page_owner`, and
completion tombstones occupy reserved memory absent from host-EL1 translation
contexts and denied to assigned-device DMA by IOMMU/SMMU policy. A platform
with no enforceable exclusion admits no guest or device; it cannot use the
tables as evidence while leaving them writable by the party they constrain.

`page_owner` is not a label that EL1 can rewrite. EL2 owns a fail-closed
assignment state machine:

```text
UnownedClean
    -> Owned(machine_incarnation, world_slot, world_generation)
    -> Quarantined(prior_or_unknown_owner)
    -> UnownedClean
```

On every cold or warm EL2 initialization, each assignable frame starts in
`Quarantined` unless EL2 has itself scrubbed it or independently verified a
hardware or cryptographic cleanliness guarantee. Missing prior ownership state
never implies `UnownedClean`, and an EL1 assertion that RAM was cleared is not
such a guarantee.

Initial assignment is allowed only from `UnownedClean`. Clearing ownership
requires the completed teardown acknowledgment for that exact owner and
generation, with no installed mapping, DMA window, or machine pin, and first
moves the frame to `Quarantined`. EL2 must then zero the frame itself or verify
a hardware or cryptographic erase whose trust does not reduce to an EL1
assertion. Only a successfully scrubbed frame becomes `UnownedClean` and may
be assigned to another World. A request to relabel an owned, mapped,
DMA-visible, pinned, or quarantined frame is rejected rather than interpreted
as transfer. Every map and ownership transition compares the requested
operation with the live EL2 tables; serialized handle fields never override
them. Thus even buggy EL1 policy cannot relabel a live PFN to evade the mapping
check, World-slot reuse cannot inherit an old generation's frames, and
reassignment cannot disclose residual contents.

O-core owns dependency traversal and guest policy. EL2 owns the mechanical
completion of each requested machine transition. Saved vCPU registers,
stage-2 roots, VMIDs, pending virtual interrupts, exit syndromes, and timer
state are continuation state, not a second capability graph.

O-core's guest-device broker is also the layer that understands virtio and 9P.
It is trusted for protocol correctness and for producing the promised guest
error. EL2 never parses a virtio descriptor, chooses a block commit point, or
constructs a 9P error. Its lower-level acknowledgment covers only machine
mapping, DMA, interrupt-route, vCPU, and generation reachability. The
class-specific O-core operation composes that acknowledgment with the broker's
protocol obligation.

## G7 caller model

G7 uses a fully virtualized, binary-contained Linux guest. The guest receives
no `MachineHandle`, no revocation-completion handle, and no Ostadix-specific
HVC or paravirtual authority/control ABI. Virtio devices use trapped MMIO or PCI
doorbells; a doorbell is not an authority-bearing hypercall. The guest agent
uses a bounded virtio transport rather than calling O-Machine directly.

Architecture platform calls needed to boot a guest, such as a narrowly
emulated PSCI operation on AArch64, carry no O-Machine handle and grant no
resource authority. They do not create a general guest hypercall ABI. Every G7
O-Machine resource operation is issued by O-core at host EL1.

The checked HVC in G2 is likewise a host EL1-to-resident-EL2 bring-up probe. It
must not be described as a guest hypercall interface.

## Machine handles

The logical G7 handle format is:

```text
MachineHandleV1 {
    abi_version
    machine_incarnation
    domain_tag
    world_slot
    world_generation
    resource_slot
    generation
    rights
}
```

`domain_tag` is a required, non-interchangeable type discriminator. Its value
space reserves EL2 domains for memory, stage-2 roots, vCPUs, interrupts, DMA,
entries, completions, and later machine-level resource classes. An opcode for
one domain must reject a handle from another domain. `MachineBlock` and
`Machine9P` are typed O-core broker resources rather than meanings interpreted
by EL2. The field is also reserved as the domain-separation input for a
possible future authenticator; G7 does not compute one.

There is no G7 `K` and no G7 handle MAC. A MAC held by EL1 would not protect
against EL1, while EL2 minting policy-bearing handles would collapse the
authority split. G7 instead relies on EL2-issued slot generations, strict
domain checks, and the independent `page_owner` check. Serialized tuple fields
are descriptive and cannot recreate a live EL2 table entry.

`world_generation` and the resource `generation` are checked nonzero `u64`
values. Neither may wrap or be reused for a slot. An exhausted slot is retired
fail-closed, including its completion namespace; allocating a numerically old
generation is never a recovery mechanism.

`machine_incarnation` is a nonzero 128-bit value selected from durable
monotonic state or a cryptographically strong uniqueness source at cold EL2
initialization. EL2 compares it with the live incarnation on every operation.
This prevents a persisted pre-reset handle or completion from aliasing counters
after a reset that lost volatile EL2 state. If neither durable monotonic state
nor sufficient uniqueness can be established, EL2 fails closed and assigns no
World resource.

G8 is the explicit decision point for any direct guest paravirtual interface.
If an untrusted guest will present machine handles, a new ABI version must
define an EL2-held or otherwise guest-inaccessible key, a construction such as

```text
MAC_K(canonical_length_prefixed(
    abi_version,
    machine_incarnation,
    domain_tag,
    world_slot,
    world_generation,
    resource_slot,
    generation,
    rights))
```

The framing must be canonical and unambiguous; raw field concatenation is not
an encoding. The ABI version is authenticated so a valid handle cannot be
reinterpreted under another layout. The new ABI must also define key generation,
enrollment, rotation, suspend/resume, migration, EL1
restart, crash recovery, and destruction rules. If G8 retains the G7 caller
model, no handle MAC or key lifecycle is required. Reserving `domain_tag` now
keeps either decision ABI-compatible without pretending that cryptography
already supplies isolation.

## Two-phase revocation ABI

The O-core machine-resource API is asynchronous even when its first
implementation is single-CPU and internally synchronous. These are typed
O-core operations, not three opcodes interpreted directly by EL2:

```text
begin_teardown_memory(memory: MachineMemory, reason, operation_id)
    -> Rejected(error) | Pending(completion_handle)
begin_withdraw_block(block: MachineBlock, reason, operation_id)
    -> Rejected(error) | Pending(completion_handle)
begin_withdraw_9p(endpoint: Machine9P, reason, operation_id)
    -> Rejected(error) | Pending(completion_handle)

query_completion(completion_handle) -> Pending
                                     | Complete(HostResourceAck)
                                     | Failed(error)
```

A valid begin operation returns `Pending`. The first one-CPU implementation may
finish all internal work before returning that result, but O-core must still
obtain the acknowledgment through `query_completion` (or a future completion
queue carrying the same typed record). This preserves the ABI when G3 adds
remote vCPU exits and cross-CPU TLB shootdown. G2 itself has no stage-2 resource
objects and implements none of these operations.

`Rejected(error)` is a pre-begin result and changes no resource, generation,
or completion state. While an accepted operation is `Pending`, repeating the
same idempotency tuple may continue safe internal work but returns the same
completion identity. A post-begin `Failed(error)` is a durable, idempotent
tombstone, not rollback: the partially withdrawn resource remains fenced or
quarantined, the old generation cannot resume, and the result authorizes no
acknowledgment, reuse, reclamation, or replacement. Recovery uses a new
class-specific operation linked to that failed tombstone and may only complete
teardown or the class reset path. The failed tombstone remains until recovery
itself is acknowledged and both records are durably consumed.

The completion handle is bound to the machine incarnation, resource class,
World slot, exact World generation, resource slot, old resource generation,
and `operation_id`. The operation ID is O-core's idempotency key within that
World generation.
Repeating the same opcode, handle, reason, and operation ID returns the same
pending or completed identity; reusing an operation ID with different inputs
is rejected. A `Failed` result is not an acknowledgment and does not authorize
reclamation or reassignment. Resource generation alone can never alias a
reused World slot.

`query_completion` resolves that immutable old tuple even after the live World
or resource generation advances. This is observation of a retained tombstone,
not live authority: the completion handle cannot issue a resource operation or
recreate the retired generation.

Completion state is a bounded tombstone stored independently of the reusable
resource slot while remaining bound to the exact old generation. Incrementing
or reusing the resource slot cannot erase it. A full completion table rejects
new begin operations without changing resource state. Operation IDs cannot be
reused while a live or retained tombstone with that World generation exists.

EL2 retains its `MachineRevokeAck` tombstone across host-EL1 restart. O-core
keeps the composite `HostResourceAck` in its own crash-consistent write-ahead
journal: it records intent before broker mutation, then records the protocol
terminal-publication receipt and the referenced EL2 completion identities
before marking the operation complete. On restart, O-core reconstructs the
broker tombstone from that journal and re-queries EL2; EL2 stores no virtio or
9P policy. A missing, corrupt, or full journal fails closed without
synthesizing an acknowledgment. The completion handle remains queryable until
the exact host acknowledgment is durably consumed and explicitly retired.
This prevents a crash in the generation-change-to-observed-ack interval from
stranding a resource or inviting reclamation without proof.

After a whole-machine reset that loses EL2 completion state, prior
acknowledgments are not reconstructed from absence. Cold initialization first
selects a fresh non-repeating `machine_incarnation`, so every pre-reset handle,
operation, and completion fails closed. Frames enter quarantine and require
trusted scrubbing before assignment; device resources require their reset/fence
recovery path. This is recovery, not retroactive proof that a pre-reset guest
consumed a protocol result.

For `MachineMemory`, `HostResourceAck` wraps EL2's `MachineRevokeAck`. For
`MachineBlock` and `Machine9P`, it is a composite O-core record: the broker has
retired its resource generation and published the protocol terminal result,
and every dependent EL2 mapping, DMA, or interrupt-route transition has a
`MachineRevokeAck`. EL2 does not produce the protocol portion.

Every `MachineRevokeAck` binds this exact tuple:

```text
abi_version
machine_incarnation
domain_tag
world_slot
world_generation
resource_slot
old_generation
new_generation
operation_id
result
```

An acknowledgment from another machine incarnation, domain, World, resource,
generation transition, or operation ID is rejected and cannot discharge a
dependency.

`MachineRevokeAck` is EL2's host-visible machine linearization record. Its
invariant is:

```text
ack(revoke(s))
    implies no machine effect authorized by s.old_generation remains reachable
```

The generation change occurs before the acknowledgment; the acknowledgment is
the externally observed linearization point. O-core must not publish reuse,
reassign ownership, or reclaim storage before it receives that acknowledgment.
A generation increment by itself is never completion.

“No machine effect remains reachable” forbids any new old-generation state
transition. Immutable terminal response bytes and an already-enqueued terminal
notification may remain for guest consumption; they are inert completed
observations, not handles, authority, or permission to touch the backend.

This ABI deliberately has no uniform semantic `revoke_resource` operation.
The begin/query framing is common, but the completion obligations and guest
consequences are defined by resource class.

## `MachineMemory` revocation

`MachineMemory` denotes an owned physical-frame set and its stage-2 exposure to
one World. It is not a recoverable guest service. Revoking it is teardown.

All dependent DMA windows must first complete their own class-specific
revocation. EL2 then completes this exact order for every vCPU and processing
element that could use the old mapping:

```text
stop or force-exit affected vCPUs and quiesce host-broker users
    -> release scoped host-broker pins and remove/disable every affected
       guest or host-broker stage-2 mapping
    -> issue the required stage-2 TLB invalidation
    -> complete the architectural drain/barriers on every affected CPU
    -> checked increment resource_generation[resource_slot]
    -> emit MachineRevokeAck
```

On AArch64, publishing the invalid PTEs, the correctly scoped TLBI, and the
required completion barriers are one indivisible revocation obligation. A
local counter update cannot substitute for the shootdown. On SMP the request
remains pending until every participating CPU has exited the old translation
context and acknowledged the drain.

`page_owner[pfn]` cannot change until the old `MachineMemory` acknowledgment
exists. After that acknowledgment, O-core may request the separate
owner-quarantine transition. EL2-controlled or independently verified scrubbing
must complete before the frame becomes `UnownedClean`; only then can it be
assigned to a new `(world_slot, world_generation)`. A stage-2 fault, injected
abort, or stopped vCPU may be the guest-side consequence, but none is a
graceful revocation result. A claim that a running Linux guest observed a
deterministic ordinary I/O error must not be based on revoking guest memory.

## `MachineBlock` revocation

`MachineBlock` denotes a generation-bound virtual block endpoint and backing
service implemented by the O-core guest-device broker. Its withdrawal contract
uses the error path already defined by virtio-blk:

1. stop accepting new descriptors for the old endpoint generation;
2. fence new backing operations and classify every accepted request at its
   backend commit point;
3. let an already committed operation report its committed result, but complete
   every accepted, uncommitted request with `VIRTIO_BLK_S_IOERR`;
4. drain old-generation backing work and obtain EL2 acknowledgments for every
   dependent mapping or DMA transition;
5. publish the terminal used-ring entries with the required virtio memory
   ordering and enqueue the terminal notification while the virtqueue memory
   remains owned and mapped;
6. fence the old interrupt route, retaining only the already-enqueued inert
   terminal notification, and obtain its EL2 acknowledgment;
7. retire the O-core endpoint generation; and
8. complete `HostResourceAck` only when both broker and EL2 obligations hold.

The broker, not EL2, chooses the commit point and writes
`VIRTIO_BLK_S_IOERR`. EL2 only certifies that the lower mapping, DMA, and
interrupt-route effects named by the broker can no longer recur.

For the pinned Linux guest used by a G7 demonstration,
`VIRTIO_BLK_S_IOERR` must be observed as the selected request returning
`EIO`. This is a property of that block-device test, not of generic resource
revocation. The pinned guest must remain healthy after returning that error.
Revoking `MachineBlock` does not implicitly revoke or reclaim the
guest's `MachineMemory`; memory teardown is a later, separate operation.
After publication, retained used-ring bytes and the terminal notification are
inert completed observations. They let the guest consume the terminal result
but grant no continuing old-generation block authority and authorize no
further backend effect. Those bytes, queue slots, and the notification are
immutable and cannot be reused until the guest consumes them or O-core chooses
explicit memory teardown, which forfeits the graceful-error claim.

## `Machine9P` revocation

A virtio-9p endpoint also has a recoverable protocol-native failure path. The
O-core broker must stop accepting old-generation tags and fids, fence backend
work, and return a valid 9P terminal error for accepted work that has not
crossed its commit point. Depending on the negotiated dialect, the wire result
is `Rerror` or `Rlerror` with the specified `EIO` or stale-object error.
Old-generation fids remain stale and are never silently rebound.

The broker publishes the terminal response or used-ring entry with the required
memory ordering while queue memory is still mapped, then enqueues its terminal
notification and obtains EL2 acknowledgments for
the dependent machine transitions and retires the endpoint generation.
`HostResourceAck` completes only after both halves hold. EL2 never parses a fid
or constructs a 9P response. The pinned guest can subsequently observe an
ordinary filesystem error, remain healthy, and continue; its later consumption is not the host
acknowledgment. Retained terminal response bytes and the already-enqueued
notification are inert completed observations, not continuing 9P authority or
permission for another old-generation backend effect. The response bytes,
queue slots, and notification remain immutable and non-reusable until guest
consumption or explicit memory teardown; teardown before consumption cannot
qualify as a delivered graceful error.

## Host completion versus guest-visible result

These are separate observations:

- **Host resource completion** is `HostResourceAck`. It contains the required
  EL2 `MachineRevokeAck` records plus any class-specific O-core broker result.
  It proves the old resource generation can no longer cause an effect and
  permits only the class contract's dependency-ordered reuse or reclamation.
  It does not authorize reclaiming guest queue memory needed to observe a
  promised terminal result.
- **Guest-visible result** is a device/protocol observation: for example, a
  pinned Linux request returns `EIO`, a 9P operation returns its negotiated
  error, or a memory teardown stops/faults the guest.

Posting a virtio or 9P completion before the host acknowledgment does not prove
that the guest has scheduled, consumed it, and returned the corresponding
errno. Conversely, observing guest failure does not prove that TLB, DMA, and
interrupt state is drained. Evidence must record both events when a gate claims
both properties.

To demonstrate a graceful failure, O-core requires both `HostResourceAck` and
the chosen guest-visible error observation before `MachineMemory` teardown.
No order is required between those two observations; each may occur first, but
both must exist before teardown begins.
The guest observation includes consumption of the used-ring/9P completion and
the pinned Linux operation returning the specified error; merely publishing
bytes or enqueueing an interrupt is insufficient. If O-core tears queue memory
down first, it has chosen World teardown and cannot claim a delivered `EIO`.

## Gate boundaries

G7 uses standard fully virtualized Linux plus trapped virtio MMIO/PCI. It has
no guest Ostadix HVC, no guest machine handle, and no Ostadix paravirtual
authority/control ABI. Its revocation demonstration selects `MachineBlock` or
`Machine9P`, records both `HostResourceAck` and the pinned guest's consumption
of the protocol-native error, and only then treats memory revocation separately
as teardown.

G8 adds a real physical-device service and must separately qualify device
quiesce, DMA-window teardown, interrupt withdrawal, reset, replacement, and
unrelated-World survival. Before freezing G8's guest-facing ABI, it must decide
whether guests ever call O-Machine directly. A yes decision activates the MAC
threat model and key-lifecycle work above; a no decision retains the smaller G7
TCB and caller model.

G8 must also choose at least one concrete physical device class and freeze a
class-named withdrawal operation rather than add a generic device revoke verb.
For example, an NVMe or network class must specify and qualify its own variant
of:

```text
quiesce new submissions
    -> stop the device or fence new DMA
    -> remove DMA mappings and drain in-flight DMA
    -> withdraw interrupt routes and drain pending interrupts
    -> reset the device to the class-defined state
    -> retire its generation
    -> complete HostResourceAck
```

The chosen class may strengthen this order but cannot silently weaken or
reinterpret it for another device family. Reset success must be verified by the
class contract. A failed or unverifiable reset leaves the device and its whole
isolation/reset group quarantined, emits no `HostResourceAck`, and permits no
replacement. A shared group is admissible only when it is dedicated to one
World or every affected World is quiesced and the gate proves their survival;
resetting a neighbor cannot be hidden beneath an “unrelated World survived”
claim.

Neither this contract nor G2 currently proves stage-2 isolation, cross-CPU
shootdown, the crash-consistent completion journal, frame
quarantine/scrubbing, virtio-blk or virtio-9p revocation, guest-visible `EIO`,
physical DMA isolation, device reset, G7, or G8.
