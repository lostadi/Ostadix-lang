# OIR Execution Fabric V1

Status: frozen M2 loopback profile. This document specifies the canonical
capsule and provisional-candidate boundary implemented by
`crates/ostadix-api/src/execution_fabric/`.

## Authority boundary

An execution-fabric worker computes a candidate result. It does not publish an
HGraph value, advance resource state, settle trace state, or commit an effect.
Those transitions remain coordinator-owned.

The coordinator is the sole graph-commit authority and the sole linearization
locus for graph transitions. Physical execution and provisionally safe pure
publication may overlap; this authority boundary does not serialize all
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
signature authority remain M3 work.

All `ExecutionFabricError` values classify as infrastructure/authority aborts,
never ordinary O-language node failures. `CandidateOutcomeV1::Failed` is only
an untrusted provisional report. The frozen M2 renderer is admitted infallible,
so even a structurally valid reported failure is rejected as a broken execution
contract. A later executor-side M3 adapter may map a validated failure to a
semantic node failure only when the admitted operation is explicitly fallible.
Malformed encoding, deadline, binding, or fidelity failures must remain
infrastructure failures. A future M3 adapter must classify stale identity,
signature, lease, and other authority failures the same way. No such M3 adapter
exists in this profile.

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

## Retry staging

M3 initially disables automatic retry after ambiguous delivery. This is an MVP
restriction, not the permanent semantic rule. A later M4 profile may retry an
admitted pure deterministic operation under a fresh attempt generation because
workers cannot commit graph state and stale attempts can be fenced. Duplicate
pure candidates must agree by canonical digest; disagreement is a broken
execution contract. Idempotent or prepared effects require their own stable
effect/transaction contracts, while opaque, irreversible, or unknown-effect
ambiguity remains fail-closed and may become `Indeterminate`.

## Explicit nonclaims

V1 does not provide a network endpoint, remote node dispatch, placement,
one-use leases, retries, cancellation, stale node-generation fencing, artifact
transfer, distributed HGraph settlement, governed hardware lowering, stateful
actors, durable journaling, or exactly-once effects. The production coordinator
continues to use `LocalWorkerDriver`, which wraps the existing persistent
`WorkerPool` without changing its settlement semantics.

Those capabilities require later profiles and must not be inferred from the M2
loopback evidence.
