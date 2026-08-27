# OIR Execution Fabric V1

Status: frozen M2 loopback profile with an implemented additive M3
authenticated remote-execution boundary. This document specifies the canonical
capsule and provisional-candidate boundary implemented by
`crates/ostadix-api/src/execution_fabric/`; M3 does not mutate those records.

## Authority boundary

An execution-fabric worker computes a candidate result. It does not publish an
HGraph value, advance resource state, settle trace state, or commit an effect.
Those transitions remain coordinator-owned.

The coordinator is the sole graph-commit authority and the sole linearization locus for graph transitions.
Physical execution and provisionally safe pure publication may overlap; this
authority boundary does not serialize all
computation. It preserves three distinct coordinator gates:

1. candidate validation checks the candidate against the coordinator-supplied
   capsule identity, bindings, deadline, and frozen execution/output contract;
2. candidate acceptance and publication decide whether a pure, infallible
   value may become provisionally visible to safe dependents; and
3. deterministic settlement decides whether an outcome enters the committed
   semantic trace.

The wire protocol terminates before the first gate. A worker only returns inert
candidate bytes. It cannot cross any of the three gates.

The V1 candidate therefore binds the attempt and capsule digests but carries no
worker-selected HGraph node identity. A coordinator must call
`ExecutionCandidateV1::validate_for_coordinator_acceptance` with its own nonzero
observation time before interpreting the candidate. Structural
`validate_against` alone does not establish timeliness, and successful
acceptance does not itself publish or settle anything.

## Narrow M3 claim

Fabric V1 can authenticate and execute the admitted M2 pure renderer profile on an explicitly selected `o-node`, returning a bounded provisional candidate whose graph publication and settlement remain coordinator-controlled.

The remote node may authenticate the request, validate the execution lease,
reconstruct the admitted operation, compute a candidate value, persist its own
attempt status, and return the provisional candidate. It must not mutate the
HGraph, publish an HGraph value, identify an authoritative HGraph node, choose
the winning attempt, settle trace order, advance resource versions, commit
external effects, or initiate retry. The coordinator remains the sole
graph-commit authority and the sole linearization locus for graph transitions.

## Frozen records

The profile defines two canonical-CBOR records:

| Record | Schema | Maximum encoded size |
| --- | --- | ---: |
| Execution capsule | `ostadix.oir-execution-capsule/v1` | 64 KiB |
| Provisional candidate | `ostadix.oir-execution-candidate/v1` | 16 KiB |

Decoders reject oversized input before parsing. They then decode, validate, and
re-encode the value, rejecting any byte sequence that is not the exact
canonical encoding.

`ExecutionIdV1`, `LogicalTaskIdV1`, and `AttemptIdV1` are protocol-scoped
identities. They do not alter the frozen World identity schema. Every digest is
SHA-256 with an explicit domain separator.

Values are not represented by a second Serde mirror. Each input and output
contains the exact canonical OWVALUE record plus a domain-separated content
digest. The independent OWVALUE byte corpus and hard-coded corpus digest remain
pinned by `tests/world_value.rs` and `tests/fixtures/world_value_v1.hex`; the
capsule and candidate tests separately pin the outer canonical-CBOR bytes.

## M2 executable subset

V1 intentionally admits only one source-closed trusted inline renderer:

- renderer: HTML, Markdown, LaTeX, or text;
- source: an ordered sequence of UTF-8 literals and named input references;
- input bindings: sorted and unique, with the referenced slot set matched
  exactly; renderer parts preserve source order and may repeat an input slot;
- values: canonical `OWVALUE` Core records containing null, booleans, numbers,
  text, characters, lists, records, or maps;
- output: one declared kind and fidelity within an explicit byte limit;
- time: one nonzero runtime bound and one nonzero coordinator deadline.

For M3 source reconstruction, an exact direct `$slot` token is temporarily
lowered by the parser as `OIr::Load(slot)`. That lexical representation is not
general `Load` execution authority. The provider converts only a direct
renderer-body placeholder to the already frozen input role, requires exact
role/order/slot equality, and never performs a scope or environment lookup.
Root or nested loads, computed lookups, and arbitrary OIR remain outside the
profile.

Byte blobs, tagged values, code references, object references, errors, extension
records, ambient scope, callbacks, capabilities, and external effects are
rejected. Excluding byte blobs prevents media-typed bytes from becoming an
implicit trusted-markup channel in the HTML renderer. Recursive values must
remain entirely inside the admitted portable subset.

The capsule binds:

- the protocol-scoped execution, logical-task, and attempt identities;
- the source and region digests;
- the expected OIR and execution-plan digests;
- the backend catalog and implementation digests;
- the exact input manifest;
- the output contract, limits, and deadline.

The provisional candidate repeats the capsule digest, attempt identity,
worker-reported completion time, outcome, and output contract coordinates. The
deadline is bound transitively through the canonical capsule digest rather than
duplicated as an independently mutable field. Validation rejects mismatched or
substituted attempt and capsule fields, expired deadlines, invalid output kinds
or fidelity claims, and oversized results. Current-generation, lease, and
signature authority are outside the frozen M2 record. The additive M3
authority envelope and provider supply those checks without changing this
capsule or candidate schema; coordinator-side remote acceptance remains a
separate gate.

All `ExecutionFabricError` values classify as infrastructure/authority aborts,
never ordinary O-language node failures. `CandidateOutcomeV1::Failed` is only
an untrusted provisional report. The frozen M2 renderer is admitted infallible,
so even a structurally valid reported failure is rejected as a broken execution
contract. An executor-side adapter may map a validated failure to a semantic
node failure only when the admitted operation is explicitly fallible.
Malformed encoding, deadline, binding, fidelity, stale identity, signature,
lease, and other authority failures remain infrastructure failures. The M3b
provider enforces that classification at its boundary; the coordinator-side
remote driver is additive and does not reinterpret M2 failures.

## Loopback evidence

The codec's test-only loopback proof decodes a canonical capsule, lowers only
the admitted portable values into live `OValue` instances, calls the existing
trusted renderer, and emits a canonical provisional candidate. It obtains its
own wall and monotonic time, rejects expired capsules before realization, and
rejects candidate emission when measured realization time exceeds the capsule
runtime limit. V1 does not preempt a renderer while it is running.

The tests establish the M2 equivalence claim for the supported subset:

```text
direct trusted renderer output
==
canonical capsule -> loopback realization -> canonical candidate output
```

This proves value and output-contract transport for the local loopback profile.
It does not independently reproduce the bound OIR, plan, catalog, or backend
implementation digests, and it does not prove HGraph settlement through the
capsule path.

The equivalence regression reconstructs the declared output variant and checks
the complete `OValue`, payload bytes, type name, content identity, fidelity, and
HTML literal-versus-splice escaping. It also exercises nested portable values,
Unicode, signed and large integers, binary `-0`, a pinned NaN payload, empty
values, exact renderer-part and literal-size boundaries, noncanonical encoding,
trailing data, duplicate fields, hostile container lengths, and excessive
nesting.

## Additive M3 provider boundary

M3 wraps the frozen records in canonical `FabricRequestV1` and
`FabricResponseV1` headers under the opt-in
`ostadix-execution-fabric/1` ALPN. Fabric clients offer only that ALPN. Ordinary
Hosted V1, Hosted V1/V2, and Hosted plus Mesh server configurations do not
advertise it; only explicit Fabric-enabled server builders add the route. TLS
selection fixes the decoder before application bytes are read, with no
sniffing or fallback.

Each Fabric message contains one bounded canonical-CBOR header record and one
separately length-prefixed exact payload record when the variant requires it.
Exactly one frame is permitted in each TLS write direction. The sender follows
its frame with TLS `close_notify`, and the receiver requires that authenticated
end-of-stream before using the message. A trailing byte, second frame, bounded
end-of-stream timeout, raw TCP EOF without TLS closure, noncanonical CBOR,
duplicate keys, hostile lengths, truncation, unknown tags, and payload digest
mismatches all fail closed. No Fabric suffix is interpreted as fallback Hosted
or Mesh traffic.

An authenticated TLS principal is necessary but insufficient. A configured
Fabric issuer must sign a V3 lease for the exact capsule, source closure,
attempt, target node and generation, execution-cell incarnation, runtime, and
placement bindings. The provider validates wall-clock lease expiry, including
the fixed skew tolerance, before reconstruction, again before durable
acceptance, and after the durable `Running` transition immediately before
realization. An authenticated exact duplicate may still retrieve its
principal-bound durable status or terminal bytes after expiry; expiry never
authorizes new work. The provider enforces runtime with only its own monotonic
clock. Its reported completion wall time and duration are signed evidence, not
proof that the coordinator received the result in time.

The bounded source closure lets the provider reproduce source identity,
intent, OIR, plan, the plan-referenced catalog projection, the current inline
backend implementation, realization pipeline, and exact renderer region. It
does not contain the complete V6 evidence bundle, so the provider does not
claim to recompute admission. Instead, the signed lease must bind the exact
admission digest already carried by the canonically decoded frozen capsule.

## Retry staging

M3 initially disables automatic retry after ambiguous delivery. This is an MVP
restriction, not the permanent semantic rule. A later M4 profile may retry an
admitted pure deterministic operation under a fresh attempt generation because
workers cannot commit graph state and stale attempts can be fenced. Duplicate
pure candidates must agree by canonical digest; disagreement is a broken
execution contract. Idempotent or prepared effects require their own stable
effect/transaction contracts, while opaque, irreversible, or unknown-effect
ambiguity remains fail-closed and may become `Indeterminate`.

## Same-host two-process evidence

`tests/execution_fabric_two_node.rs` launches two real `o-node` processes on
one host with separate ports, TLS identities, node identities, state
directories, ledgers, node generations, and explicit Fabric authority
enrollment. Nodes A and B both execute admitted pure attempts, and their
provisional results match direct local execution. The proof also establishes
that a lease for A is rejected by B, a wrong-node result cannot pass
coordinator acceptance, stopping a selected node yields an infrastructure
failure without invoking the local renderer, and the existing Hosted and Mesh
ALPN routes continue to function.

This is authenticated remote execution across real process boundaries on one
host. It is not evidence of distinct kernels, physical multinode operation, or
heterogeneous hardware.

## Explicit nonclaims

The frozen M2 profile alone does not provide a network endpoint, remote node
dispatch, placement authority, leases, or durability. The additive M3 provider
adds only an explicitly enabled authenticated endpoint, one-use execution
authority, source reconstruction, and a node-local replay ledger. M3 makes
these exact nonclaims:

- no arbitrary OIR region execution;
- no general `.O` distribution;
- no automatic placement;
- no capacity scheduler;
- no scope transport;
- no object plane;
- no bulk node-to-node data transfer;
- no actors;
- no external effects;
- no automatic retry;
- no coordinator crash recovery;
- no hardware-resource execution;
- no GPU or camera driver mediation;
- no process migration;
- no shared address space;
- no physical multinode claim;
- no distinct-kernel claim;
- no heterogeneous-architecture claim;
- no exactly-once external effect claim.

M3 also adds no cancellation mechanism and no distributed HGraph settlement.
None of those capabilities may be inferred from either the M2 loopback or the
M3 authenticated provider. Graph mutation, publication, winner selection,
settlement, resource-version advancement, effect commitment, and retry remain
outside remote-node authority.
