# Local Authority and Requester Routing Amendment

Status: **proposed additive architecture clarification; not a normative
contract or evidence gate**. Mode 31 implements one bounded local routing
mechanism described below, not this complete authority amendment.

This document does not modify or supersede the byte-sealed
[`OSTADIX_WORLD.md`](OSTADIX_WORLD.md),
[`O_MACHINE_CONTRACT.md`](O_MACHINE_CONTRACT.md), or their versioned schemas.
The sealed documents remain normative. The requirements below become normative
only through a versioned successor contract and append-only evidence update.

## Existing boundary retained

The sealed constitution already keeps globally nameable resources physically
local, separates the 9P-derived namespace/control plane from capability
authority, execution, and bulk data, and keeps ordinary local execution and
bulk transfer off the replicated Governor log. It also keeps names separate
from authority and makes a fid stale when its bound generation is replaced.
The sealed O-Machine contract separately requires resource-class-specific
completion and revocation behavior rather than a universal revoke operation.

This amendment preserves those boundaries. In particular, it does not make a
path lookup or a 9P fid into authority, route bulk data through consensus, or
give every resource class filesystem retry semantics.

## Proposed responsibility split

The next versioned contracts should make the following scopes explicit:

1. **One local O-Machine authority per physical node.** It owns that node's
   machine-enforcement mechanisms (EL2 on AArch64, or the corresponding
   architecture-specific machine root) and physical machine truth. World
   admission governs the node's participation and exports, not the existence
   of its local machine root. The O-Machine has no replicated-consensus,
   global-namespace, or requester-route-selection role. A `MachineHandle` is
   valid only at the issuing O-Machine and for its recorded machine incarnation;
   it is not transferable to another node.
2. **One local O-core policy authority per node.** It decides which admitted
   local principal may receive, export, fence, or revoke a local machine
   resource, subject to current globally admitted authority. It does not decide
   global membership or commit facts. Local fencing need not wait for
   consensus, but a remote observer may not infer physical reclamation from a
   timeout or namespace change.
3. **A requester-local router.** For a noncommitting operation, the requester
   chooses among the currently known provider routes without placing that
   choice in the Governor log. A minimum admissibility predicate is:

   ```text
   GloballyAdmitted
     && GenerationCurrent
     && LocallyReachable
     && CapabilityValid
     && EffectSafe
   ```

   The decision and its exclusions belong to the request attempt. A replacement
   provider receives a fresh provider-local session, attach, walk, open, and
   fid; a fid is never migrated or rebound silently.
4. **Replicated Governor authority for globally singular facts only.** The log
   remains responsible for membership, epochs, capability roots, resource
   admission, globally visible namespace mutation, mutable ownership transfer,
   global task commitment, and evidence checkpoints. It is not on the data path
   for every 9P read, local schedule decision, bulk transfer, or requester-local
   route choice.

These are distinct authorities. A globally admitted route can still be locally
unreachable; local reachability does not create global admission; and a local
route choice cannot mint or refresh a capability.

## Failure, revocation, and reclamation facts

Implementations and evidence must report these facts separately:

1. a requester excluded a route locally;
2. a time-bounded lease expired;
3. the owner O-core completed authority revocation; and
4. the owner O-Machine acknowledged physical reclamation or an explicitly
   stronger external fence supplied equivalent evidence.

A timeout or partition can establish the first fact and, under the lease's
clock assumptions, the second. It cannot establish owner-side revocation or
physical reclamation. A Governor namespace mutation records global recognition;
it is not a substitute for the owner acknowledgement required by the last two
facts.

## 9P routing and effect semantics

9P supplies one namespace and routing algebra: logical names, provider
candidates, generations, and provider-local sessions. Capability validation
remains a separate authority step. Resource operations retain their own effect
algebras:

- an immutable, content-addressed read may retry on another admitted provider
  when every attempt validates the same object identity and digest;
- a mutable write requires resource-specific commit, replay, and idempotency
  rules before retry can be called safe;
- block, device, stream, tensor, and packet operations keep their native
  terminal-error and reclamation contracts; and
- a namespace withdrawal or stale fid does not by itself prove that an effect
  did not commit.

Thus a uniform 9P envelope does not imply uniform retry, commit, or revocation
semantics: one namespace/routing algebra carries several effect algebras.

## Evidence required before implementation claims

At minimum, a qualifying successor must provide:

- a versioned World/O-Machine contract amendment that states the local scopes
  and `MachineHandle` nonportability above;
- an executable requester-router gate with two independently admitted
  providers, fresh provider-local reconstruction, and causal attempt evidence;
- distinct observations for route exclusion, lease expiry, owner revocation,
  and physical reclamation; and
- live receipt binding if an `OWRECEIPT` is claimed. The present Mode 30 corpus
  is an offline codec/signing oracle and is not live routing evidence.

Mode 31 now supplies the bounded local requester-routing mechanism named by the
second bullet: two generation-distinct provider principals are admitted before
one immutable LogicalRead, A is excluded after a valid failure, and B completes
with fresh provider-local state. It does not make this proposed amendment
normative, distinguish provider implementations, exercise leases or remote
owners, or supply a persisted trace/live receipt. The current Mode 26 gate is
still explicitly excluded: it replaces one server
implementation across generations and performs two different completed reads;
it does not exercise a two-provider route set or recover one logical read on a
second provider.
