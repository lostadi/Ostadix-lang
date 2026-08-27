# M3 authenticated pure remote execution: internal design note

Status: implementation boundary for M3. This note is not a protocol
specification and does not advance any M4 or later claim.

## Authority invariant

An M3 provider authenticates, authorizes, reconstructs, and computes one
already admitted pure operation. It returns a provisional candidate. It has no
HGraph coordinate and no graph mutation, publication, winner-selection,
settlement, retry, resource-version, or effect-commit authority.

The coordinator remains the sole graph-commit authority and the sole
linearization locus for graph transitions. Candidate validation, provisional
publication, and deterministic settlement remain distinct coordinator-owned
transitions.

## Existing M2 records retained unchanged

| Existing record or seam | Unchanged M3 role |
| --- | --- |
| `ExecutionIdV1`, `LogicalTaskIdV1`, `AttemptIdV1` | Stable wire attempt coordinates. They are never replaced with `TaskToken`, a graph node, an operation index, or a PID. |
| `SourceClosedRendererV1` and `RendererPartV1` | Exact source closure for `TrustedInlineRendererV1`, preserving verbatim and spliced roles. Narrow read-only accessors may be added; canonical fields do not change. |
| `PortableValueV1` | The sole portable input representation: exact canonical OWVALUE bytes plus the existing digest and allowlist checks. |
| `InputManifestV1`, `OutputContractV1`, `ExecutionLimitsV1` | Frozen admission bindings for input, output kind/fidelity, absolute capsule deadline, and maximum runtime. |
| `ExecutionCapsuleV1` | Frozen semantic payload. Its known-answer bytes remain authoritative and contain no transport, placement, TLS, graph, or node state. |
| `ExecutionCandidateV1` | Frozen authority-free provisional computation record nested inside an authenticated terminal receipt. |
| M2 canonical CBOR codecs | Exact bounded capsule/candidate codecs. M3 calls them and does not create another OWVALUE or capsule encoding. |
| M2 loopback adapter | Test oracle only. Production realization is introduced outside the M2 protocol module. |
| `AttemptDriver` five-method seam | Coordinator-facing execution seam. M3 preserves its method count and uses private queues and transport workers. |
| `TaskToken` | Process-local coordinator key. It is retained only in a private remote-attempt map and never crosses the wire. |
| Hosted V1/V2 and Mesh V1 ALPN routes | Existing routes and decoders remain exact and independent. |
| `NodeProfileV1`, `CapacityObservationV1`, `PlacementReservationV1`, `TargetDescriptorV1`, eligibility, footprint, warrant, trust, admission, and backend identities | Reused placement facts and digests. M3 does not duplicate their meanings. |
| `PlacementLeaseV2` and Hosted signed envelopes | Frozen Hosted/state authority. They remain valid for Hosted V2 and are never reinterpreted as Fabric authority. |
| Existing canonical signing preimage and Ed25519 key utilities | Shared cryptographic substrate after extraction to a lower reusable helper; existing Hosted V2 signed bytes remain unchanged. |
| Hosted V2 durable-store primitives | Filesystem safety pattern to reuse: private roots, no-follow opens, exclusive root lock, canonical records, atomic rename, file and directory sync, and poison-on-corruption. Hosted schemas are not reused. |

## New M3 records and why they are distinct

| New record or component | Why no existing record represents it |
| --- | --- |
| Additive `PlacementLeaseV3` | V2 authorizes a Hosted command and optional state session. Fabric authority instead binds the exact M2 attempt, capsule/source/input/output/backend identities, runtime, provider incarnation, and TLS principal. Mutating V2 would change its signed meaning. |
| `SignedExecutionLeaseV3` | Carries the V3 canonical lease under a Fabric-only signing domain and trusted issuer key. A paired TLS certificate alone is not execution authority. |
| `FabricSourceClosureV1` | Binds the exact UTF-8 source, the `ostadix-source-closure/v1` dialect, the single root operation, base policy, and independently recomputed intent, OIR, plan, and closure digests. M2's renderer record alone does not retain the source needed for re-lowering on another node. |
| `FabricSubmissionV1` | Authenticated outer envelope containing exact `ExecutionCapsuleV1` bytes, the bounded source closure, and the signed lease without adding remote fields to M2. |
| `FabricRequestV1` | Narrow tagged request family: `SubmitPureAttempt` and `QueryAttempt` only. |
| `FabricResponseV1` | Narrow tagged result family: `Accepted`, `Running`, `TerminalCandidate`, `Rejected`, and `Abandoned` only. |
| `TerminalCandidateReceiptV1` and `SignedTerminalCandidateReceiptV1` | Binds an inert M2 candidate to the producing node, both generations, attempt, nonce, capsule/input/output contracts, output digest, runtime evidence, and terminal status under a Fabric-result signature domain. Hosted receipts have different authority and semantics. |
| `ExecutionCellIncarnationV1` | Existing node generation is a durable deployment/state epoch and is stable across ordinary restart. M3 needs a second durably incremented provider-start coordinate to fence incomplete pre-restart work truthfully. |
| `FabricAttemptLedgerV1` | Provider-only replay/fencing state. It is neither the Hosted V2 session store nor the deferred M4 coordinator journal. |
| `TrustedInlineRealizerV1` | Production reconstruction and rendering after authentication and binding checks. It lives in the provider layer because the authority-free M2 protocol is not an evaluator. |
| `RemotePureAttemptDriver` | Explicitly selected transport-backed implementation of the existing driver seam. It privately maps the remote attempt to `TaskToken` and never publishes or settles a graph result. |

## Frozen and additive schema boundary

`ExecutionCapsuleV1`, `ExecutionCandidateV1`, `PlacementLeaseV1`, and
`PlacementLeaseV2` remain byte-for-byte compatible. M3 adds V3 placement
authority; it does not add a field to a frozen record. The existing known-answer
vectors remain pinned.

The new Fabric ALPN is exactly `ostadix-execution-fabric/1`, following the
existing `ostadix-<protocol>/<version>` convention. Fabric is opt-in on both
ends. A Fabric client advertises only this ALPN. The ordinary Hosted V1,
Hosted V1/V2, and Hosted V1/V2 plus Mesh server builders do not advertise it.
Only the additive Fabric-enabled builders add it to the exact Hosted route set
that was otherwise requested. The selected ALPN chooses exactly one decoder
before any application byte is read. Missing or unknown ALPN, malformed
framing, wrong schema, or decode failure closes that route without sniffing or
fallback.

## Wire shape and canonicality

Fabric application traffic uses one bounded canonical-CBOR header frame plus,
where required, one separately length-prefixed exact payload frame. The header
binds payload length and SHA-256. Capsule and candidate payloads are then passed
unchanged to their frozen M2 bounded decoders. This prevents a generic CBOR
`Vec<u8>` from becoming a second, integer-array representation of opaque bytes.

Every header is decoded with explicit depth, item, string, and allocation
limits, re-encoded canonically, and required to equal the received bytes.
Fabric V1 carries exactly one frame in each TLS write direction. After writing
that frame, the sender emits TLS `close_notify`; the receiver requires that
authenticated end-of-stream before using the decoded message. A trailing byte,
a second frame, a peer that does not finish its write direction before the
bounded timeout, and raw TCP EOF without TLS closure all fail closed, as do
duplicate map keys, noncanonical integers/maps, unknown fields, unknown tags,
oversized declarations, truncation, and digest mismatches. No Fabric suffix is
treated as fallback Hosted or Mesh traffic. OWVALUE remains nested only through
`PortableValueV1`, which validates,
decodes, canonically re-encodes, compares exact bytes, verifies its digest, and
applies the frozen M2 value allowlist.

## Authority and currentness

A valid submission must establish two independent facts:

1. the TLS connection presents the pinned client principal bound by the lease;
2. a configured trusted Fabric issuer signed the exact V3 lease.

The V3 lease binds protocol and issuer, a one-use nonce, the complete M2
attempt coordinate, target node and stable node generation, execution-cell
incarnation, target descriptor, profile/capacity facts and generations, source,
OIR, plan, input, output, the plan-referenced backend-catalog projection,
backend implementation, realization, admission, trust/reservation bindings,
maximum runtime, issue/expiry window, and one-use requirement. The provider
checks absolute wall-clock lease validity, with the fixed two-second skew
tolerance, before reconstruction, immediately before durable acceptance, and
after the durable `Running` transition immediately before realization. A
trusted exact duplicate may retrieve its principal-bound durable status or
terminal bytes after expiry, but expiry can never authorize new work. Runtime
is measured only with the provider's local monotonic clock,
both around direct rendering and around the complete realizer call. The M3
trusted-inline renderer is a bounded, infallible direct renderer, so this
delivery rejects an over-budget result after the call returns; it does not
claim hard preemption, cancellation, or isolation. Provider completion wall
time and monotonic duration are signed evidence. They are not compared as
cross-machine monotonic time, and neither proves final timeliness. Coordinator
receipt time decides that question.

The provider can fence durable nonce and binding reuse. It cannot learn that a
coordinator has superseded an otherwise valid attempt without a revocation
oracle, which M3 deliberately does not add. The coordinator therefore rejects
stale, duplicate, fenced, already committed, and superseded candidates against
its private active-attempt state.

Direct trusted rendering is not a `KernelWorld`. M3 neither binds nor claims a
KernelWorld execution region.

## Source reconstruction and implementation identity

`FabricSourceClosureV1` retains the exact source bytes, raw-source digest,
`ostadix-source-closure/v1` dialect token, one root operation
(`root_operation == 0`), a canonical existing `Policy` spelling, and the
admitted intent, OIR, plan, and closure digests. After authentication and lease
checks, the provider parses that exact source and independently reconstructs
one `Exec` operation. Its body may contain only text literals and exact direct
`$slot` lexical placeholders. The parser transiently represents such a
placeholder as `OIr::Load(slot)`, but the realizer never executes it as a scope
or environment load. It converts that direct child only into the frozen
`RendererPartV1::Input` role and requires exact role, order, slot, and literal
equality. A root `Load`, a nested `Load`, a computed lookup, and every arbitrary
OIR subgraph remain rejected.

The provider then re-lowers through the current OIR/planner path. The frozen M2
decoder independently validates the exact capsule bytes, canonical OWVALUE
records, input manifest, output contract, and capsule digest. The source
realizer independently recomputes the source, intent, OIR, plan,
plan-referenced backend-catalog projection, current backend specification,
backend implementation, realization pipeline, and exact renderer region. The
source closure does not carry the complete V6 evidence needed to reproduce the
admission compiler. Therefore the provider does not claim to recompute the
admission digest: it requires the trusted issuer's signed V3 lease to bind the
exact admission digest already present in the frozen capsule. Any reproducible
binding mismatch precedes execution.

The trusted-inline backend implementation identity is truthful: its adapter
artifact digest covers the exact embedded realizer source set; its executable
set digest identifies the explicit empty external-executable set; its protocol
ABI and realization-pipeline domain name the in-process trusted-inline V1
implementation. It is not relabeled as a shim, proxy, shell executable, or
KernelWorld implementation.

## Provider ledger and restart fencing

The provider owns a distinct private `fabric-v1` state root and exclusive lock.
At provider startup it atomically increments and syncs
`ExecutionCellIncarnationV1`. Attempt records are canonically encoded and move
through:

```text
Received -> Validated -> Accepted -> Running -> TerminalCandidate
    \-----------> Rejected <-------------/
incomplete state from an older incarnation -> Abandoned
```

The critical durable transition atomically consumes the issuer/attempt/nonce
and stores the exact accepted binding before realizer invocation. Identical
duplicates return current state or exact stored terminal bytes and never invoke
the realizer twice. Reusing a nonce or attempt coordinate with a different
binding is replay/tamper rejection. An incomplete record from an older provider
incarnation becomes `Abandoned`; it is never resumed. A terminal record remains
attributable to its original stable node generation and provider incarnation.

The ledger is explicitly not coordinator crash recovery. M3 performs no
automatic retry. A future M4 policy may retry admitted deterministic pure work
under a fresh attempt generation and nonce.

## Coordinator bridge and failure semantics

Remote execution is opt-in. `RemotePureAttemptDriver` owns bounded worker
queues, keeps `TaskToken` local, submits to one exact configured provider,
queries when needed, validates the authenticated response, and emits an inert
internal candidate event. The coordinator admits that event to its existing
provisional-candidate path only after the ordered nineteen checks required by
the M3 delivery: canonicality, node/channel, receipt key, execution/task/attempt,
both generations, signed lease/issuer/nonce, capsule/source/input/catalog/
backend/output-contract bindings, output kind/fidelity/content, coordinator
deadline, and current unsuperseded state.

Connection, TLS, authority, currentness, framing, digest, deadline, and output
contract failures map to infrastructure/authority aborts, not O-language
semantic errors. A selected remote failure never invokes the local renderer.

## Module and commit split

- M3a: narrow M2 accessors; reusable signing helper; additive placement V3;
  `execution_fabric_authority` records, codecs, signatures, and known answers.
- M3b: Fabric ALPN/framing; `hosted_remote::fabric` realizer, provider, and
  durable ledger; explicit `o-node` configuration.
- M3c: `RemotePureAttemptDriver`, private token mapping, ordered coordinator
  validation bridge, and no-fallback failure mapping.
- M3d: two-process proof, equivalence/tamper/replay/restart/ALPN regression
  coverage, claims and version surfaces, architecture/AOT/source-release
  inventories, and executable evidence.

No layer in this split introduces cancellation, arbitrary OIR, object transfer,
capacity scheduling, actors, effects, retry, coordinator journaling, hardware
execution, migration, or graph authority on the provider.
