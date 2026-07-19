# Personality Foreign-Process Memory-View Protocol

Status: required design contract for the planned personality-service RPC. It
must be implemented and tested before a translated Linux personality can
accept pointer-bearing syscalls.

## 1. Security invariant

A personality service never receives, stores, or dereferences a raw pointer
from a foreign process. The kernel interprets every foreign address in the
target process's current, generation-tagged address space and exposes only a
bounded, request-scoped memory-view capability.

```text
foreign syscall
    -> validate process, address space, ranges, directions, and quotas
    -> copy or pin the approved bytes
    -> issue a temporary memory-view capability
    -> personality operates on that view
    -> kernel validates and commits declared output
    -> revoke the view and release pins or copies
```

A service-local mapping address is only an address in the service's own address
space. It never becomes a reusable alias for the foreign virtual address.

## 2. Request identity

Every forwarded syscall has a kernel-generated request ID and records:

- generation-tagged process, thread, and address-space handles;
- personality service generation and protocol version;
- syscall number and immutable scalar arguments;
- one or more checked memory segments;
- deadline, cancellation state, and resource charges;
- restart class and side-effect commit state; and
- the endpoint generation that owns the reply.

A segment records foreign address, byte length, direction (`in`, `out`, or
`inout`), transfer mode, maximum committed length, and required mapping rights.
Range addition is checked for overflow. A nonempty segment must fit mapped user
pages with the required permissions. Overlapping output segments are rejected
unless the ABI adapter defines and tests an explicit ordering rule.

Request IDs prevent reply confusion. They do not by themselves make a syscall
safe to repeat.

## 3. Transfer modes

### Bounded copy

Bounded copy is the default. Input bytes are copied into kernel-owned staging
storage and then into a service-owned view. Output remains staged until commit.
This provides snapshot semantics and avoids holding target pages across a
blocking service operation. Per-request and per-domain byte quotas are hard
limits.

### Pinned window

Pinned windows are an optimization for large, shared, or explicitly streaming
operations. The kernel pins the exact page set and maps an attenuated view into
the service. The service receives only the requested direction and interval.
Unmap, protection change, process teardown, and capability revocation join the
same pin state machine instead of racing the mapping.

Pinned output that can become visible before reply must be explicitly declared
as streaming. Ordinary output uses staged commit even if the backing pages are
pinned.

The initial Linux proof should use bounded copies wherever the pinned semantics
are not required by its pinned syscall corpus.

## 4. Memory-view capability

A memory-view handle is generation-tagged and scoped to exactly one request.
Its object records owner endpoint generation, target address-space generation,
segments, rights, expiry, pin or staging references, and lifecycle state.

Possible rights are:

- `read_input`: read staged or pinned input;
- `write_output`: write no more than a segment's output bound;
- `commit_output`: propose a completed byte count and segment status; and
- `stream_output`: modify explicitly shared output before final reply.

Rights are not implied by the foreign process's mapping. They are independently
attenuated for the personality operation. The service cannot transfer a view,
extend it, change its target, or retain it after terminal request disposition.

## 5. Lifecycle and terminal results

The kernel owns this state machine:

```text
created -> dispatched -> running -> reply_pending -> committed
                         |    |            |
                         |    +-> cancelled+
                         +------> failed---+
```

Exactly one terminal disposition is recorded: `committed`, `cancelled`, or
`failed`. Recording a terminal disposition revokes the view before waking the
foreign thread. Late and duplicate replies fail with a stale-request error.

For staged output, commit validates segment identity and byte counts, copies
only the declared prefix back through fault-aware kernel copy routines, and
reports a defined partial-result error if a destination became invalid. The
ABI adapter maps that result to the personality's return and error convention.
No unwritten staging bytes are exposed.

## 6. Failure and concurrency rules

### Personality crash or restart

Service death revokes all views owned by that service generation. Requests with
no external side effect may be redispatched only when their adapter marks them
restart-safe. A request that may have performed an external effect becomes
`failed_indeterminate` unless its operation protocol supplies an idempotency
token and durable commit receipt. A restarted service cannot reply to the prior
generation's request IDs.

### Foreign process or thread exit

Exit marks every owned request cancelled, revokes its views, releases pins, and
discards staged output. Teardown waits only for kernel-held references, not for
service cooperation. The service observes cancellation through its endpoint
and any later reply is rejected.

### Unmap and protection change

Bounded-copy input is unaffected after the snapshot. Staged output is checked
again at commit. A pinned window makes unmap or incompatible protection change
wait for, cancel, or explicitly detach the request according to the adapter's
declared blocking policy. It may never free a pinned frame underneath the
service.

### Blocking syscalls and cancellation

A blocking request parks the foreign thread with a recorded wake reason. Timer,
signal, explicit cancellation, service death, and normal reply race through one
atomic terminal transition, so the thread wakes exactly once. Cancellation
does not promise that an external side effect was undone.

### Signals and restart

Signal delivery records whether the ABI adapter returns an interrupt error,
restarts the call, or delivers after completion. Restart creates a new request
unless the original never reached `running`. The Linux adapter must compare
this behavior with a pinned native Linux oracle.

### Capability revocation

Revoking a delegated filesystem, network, timer, device, endpoint, or memory
authority prevents new use immediately and moves affected requests through the
same cancellation transition. Revocation never falls back to ambient access.

### Shared buffers and partial writes

Shared buffers require an explicit shared-memory capability and concurrency
contract. They are not smuggled through ordinary pointer arguments. Partial
writes return a committed byte count only for bytes the kernel verified and
made visible. The remaining range stays unchanged unless the foreign ABI
explicitly specifies otherwise.

## 7. Resource controls

The kernel charges staging bytes, pinned pages, view objects, queued requests,
and service CPU time to both the caller domain and personality service. Hard
limits bound segment count, total bytes, pin duration, nesting, and outstanding
requests. Exceeding a limit returns a defined resource error before dispatch.

Personality services cannot ask the kernel to log arbitrary foreign memory.
Diagnostics use bounded, redacted metadata unless a separately authorized
debug capability is present.

## 8. Acceptance gate

Before pointer-bearing Linux syscalls are accepted, executable tests must show:

- range overflow, kernel addresses, stale process/address-space generations,
  wrong mapping rights, excessive segments, and excessive bytes fail before
  dispatch;
- a service cannot read an output-only view, write an input-only view, extend a
  segment, transfer the handle, or use it after reply;
- unmap, process exit, signal, timeout, explicit cancellation, service crash,
  reply, and capability revocation races produce one terminal result and one
  wakeup;
- staged output never exposes uncommitted or unwritten bytes;
- pinned frames are neither freed nor remapped while a view is live and are
  reclaimed after every terminal path;
- duplicate and prior-generation replies are rejected;
- partial copy and partial external-effect cases map to the pinned ABI oracle;
  and
- unrelated native processes continue through service crashes and hostile
  request corpora.

Fuzzing must cover the serialized request/reply schema and state transitions.
Fault injection must cover allocation failure at every staging, pinning,
dispatch, reply, and commit step.

## 9. Explicit non-claims

This protocol does not make arbitrary device DMA safe, make every syscall
restartable, provide distributed exactly-once execution, or turn serialized
capability metadata into authority. Direct device passthrough requires an
IOMMU-backed isolation design and separate acceptance evidence.
