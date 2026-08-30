# Information Kernel V1

Status: experimental, local-first, authority-free sidecar

The Information Kernel V1 gives Ostadix one content-addressed vocabulary for
identity, provenance, projections, decisions, and observations without
replacing the existing execution authorities. It is deliberately additive:
existing OIR, evidence, admission, placement, Hosted, World, and O-core record
bytes retain their current identities.

## Boundary

An information snapshot says which facts are present. It does **not** say that
every fact is true, current, mutually consistent, causally related, or
authorized for execution. Existing admission and placement records remain the
only execution authority.

## V1 byte compatibility and the V2 provenance sidecar

The provenance extension is additive. `InformationAtomV1`,
`AcquisitionModalityV1`, their canonical CBOR encodings, and the
domain-separated `AtomIdV1` digest remain byte-for-byte unchanged. The same is
true of V1 snapshots, revisions, deltas, signed packs, and every identity that
transitively binds those bytes. A V1 atom is never rewritten in place to add a
witness or to change its acquisition modality.

Instead, V2 provenance is a separate sidecar bound to the existing
`AtomIdV1`. Raw witness shapes live inside the low-layer information root;
contextual analysis and opaque admitted handles live in the higher-layer
`information_provenance` root described by
[`IMAGE_ADMISSION.md`](IMAGE_ADMISSION.md). This keeps the V1 information
kernel and the authority-free `information_bridge` free of execution or
evidence authority. The existing `o-info` V1 import path remains bounded to
declared atoms; it does not silently uplift V1 records into admitted V2
provenance.

The V1 implementation currently provides:

- domain-separated identifiers for entities, atoms, snapshots, revisions,
  projection receipts, deltas, decisions, and observations;
- immutable snapshots separated from revision lineage;
- declared, derived, enforced, observed, predicted, counterfactual,
  contradicted, and invalidated modalities. Contradiction and invalidation are
  provenance-bearing atoms; they never silently overwrite the earlier claim;
- scoped facts with explicit producer and support sets;
- projection receipts binding an exact source root, read set, projector,
  output, loss contract, freshness requirements, and lift schema;
- compositional loss contracts;
- exact-base information deltas with conflict-preserving reconciliation;
- dependency-driven projection invalidation;
- deterministic, bounded, read-only value-of-information selection;
- a private local content-addressed store with an exclusive root lock and
  exact compare-and-set heads;
- canonical, signed offline delta packs whose signer is checked against an
  independent local trust resolver.

## Typed native bridge

Package 0.3 adds the experimental `information_bridge` leaf root. It projects
only eight allowlisted, read-only records into T2-shaped canonical metadata;
there is no generic `Serialize` entry point, native reverse edge, authority
conversion, mutation, dispatch, credential, capability, admission, or
execution handle.

| Native input | Bridge record | Exported metadata only |
| --- | --- | --- |
| `ParsedDocumentV1` plus exact source bytes | `ParsedDocumentInformationV1` | source SHA-256/length, immutable syntax/origin counts, origin-map digest |
| caller-declared-public scalar `OValue` | `PublicValueInformationV1` | allowlisted scalar kind, canonical digest/length, caller-public flag |
| unadmitted `HGraph` | `HGraphInformationV1` | structural counts and `metadata_projection_sha256` over exactly those counts |
| Evidence V6 | `EvidenceInformationV1` | exact evidence schema/analyzer, Catalog V6 public projection digest, node count, metadata projection digest |
| verified registry profile | `RegistryProfileInformationV1` | canonical namespace, projected node identity, generation, event digest, validity range, stale flag |
| verified World receipt | `WorldReceiptInformationV1` | receipt/semantic digests and `signature_validated=true` |
| logical project HGraph | `ProjectGraphInformationV1` | logical/source-bundle digests and operation/root counts |
| self-signature-consistent Hosted V2 journal entry | `HostedJournalInformationV1` | projected session/current/previous entry identities, sequence/time, self-consistency, explicit `signer_trust_evaluated=false` |

Every record has its own `ostadix.info-bridge-*/v1` schema, strict canonical
decoder, semantic validator, and digest-only `NativeRecordRefV1`. Decoding
checks bytes and declared invariants; it does not establish supplier trust.
T2 references contain no native locator or inverse lookup mechanism.

The parser captures exact source SHA-256 and byte length. A parsed-document
projection fails unless supplied bytes match both. `ParsedDocumentV1.nodes` is
private; `nodes()` and `into_nodes()` are the supported borrowed and owned
accessors. Derived equality now also includes captured source digest and length.
Scalar projection preflights size, numeric recursion/node count, bigint
magnitude, rational denominator, binary-float width, and BigFloat precision
before canonicalization. Container, path, byte/blob, graph, native, request,
system, capability, snapshot, thunk, group, and error values are rejected.

HGraph and Evidence `metadata_projection_sha256` values hash only the fields in
the table. Inputs with the same allowlisted metadata intentionally collide even
when omitted source, value, OIR, plan, native graph/evidence identity, runtime,
environment, capability, placement, or dispatch fields differ. These digests
are neither native identities, privacy commitments, nor authority. HGraphs
with an admission map or any admission-evidence node are rejected.

Registry namespaces remain raw because their bounded slash-separated grammar
is a public descriptive scope, not a locator or secret classifier. Raw
registry node IDs and Hosted session/journal-link tokens are removed by
bridge-specific, length-framed, domain-separated SHA-256 projections. These
projections preserve equality (and Hosted link equality), but remain dictionary
oracles for low-entropy inputs; they are not confidentiality or privacy
primitives. Registry `stale` is descriptive only. World
`signature_validated=true` proves neither freshness nor authorization. Hosted
self-consistency proves neither signer trust, journal continuity, nor
currentness.

The independent `ostadix_api::api` engine surface exports the explicit bridge;
`o_lang::api` is an exact compatibility reexport of that same module and type
identity. Generated AOT projects intentionally embed neither `information` nor
`information_bridge`; their one-copy runtime closure remains separate.

## Payload tiers

Canonical information records have no typed variants for bearer tokens,
private keys, live capabilities, executable handles, secret retrieval
locators, or credentials. This is a type boundary, not content inspection:
arbitrary T0 text could still contain a secret if a caller misclassifies it.
The local CLI therefore requires `--acknowledge-public` before recording T0.

| Tier | Meaning | V1 bound |
| --- | --- | --- |
| T0 | Small public typed scalar embedded in the atom | 4 KiB canonical bytes |
| T1 | Local managed non-secret blob reference | 16 MiB logical bytes |
| T2 | External record schema, media type, digest, and length | bytes remain outside the information store |

T2 deliberately contains no path, URI, native retrieval identity, or
credential-like locator. Retrieval credentials and secret locators belong to
an authority-bearing runtime channel, never to a canonical information atom.

## Projection closure

A materialized view is acceptable only when it carries a projection receipt.
The receipt binds:

1. the immutable information root and exact facts read;
2. the projection recipe, implementation, configuration, and direction;
3. the projected output and stable canonical-to-local identity map;
4. the distinctions omitted or made uncertain;
5. the scope, freshness condition, consumer contract, and lift schema.

A component returns new knowledge as a delta anchored to that receipt and the
revision from which it worked. A stale delta is retained as historical
information; it is not silently promoted to current state.

## Local and offline operation

V1 needs no daemon, cloud account, consensus service, or laboratory cluster.
The store is a local directory, and offline packs are canonical CBOR signed
with Ed25519. Pack verification keeps four judgments separate:

- the pack envelope is canonical and each included object is digest-bound;
- the signature is cryptographically valid;
- the signer is accepted by an independent local trust policy;
- the contained claim is semantically valid and operationally applicable.

Only the first three belong to pack verification. A verified pack does not
become an execution lease or admission.

The V1 offline boundary is intentionally laptop-sized and closed under its
decoder limits: at most 1,024 packed objects, at most 256 KiB for any one
object and for the sum of packed object bytes, at most 768 KiB for the
canonical inner pack, and at most 1 MiB for the signed envelope. These are hard
ceilings even when a caller supplies a looser policy. The local store uses the
same 256 KiB bound for canonical record objects and 1 MiB bound for historical
packs, while separately managed T1 blob content retains its exact 16 MiB
capacity.

`InformationStoreReaderV1::open_existing` is the separate inspection path for
an already initialized private root. It creates no directory or lock file,
repairs no mode, and performs no head update. Its bounded regular-file reads
verify object identity. `o-info head` uses this reader; tests prove no change to
directory entries, file content, inode, mode, or mtime. Atime is not part of
that claim. Missing, nonprivate, or symlinked roots/kind directories fail
without repair. This path does not claim hostile same-user resistance if an
attacker can replace ancestor directories concurrently.

The bridge decoder ceilings are conjunctive safety bounds: 64 KiB of input,
256 decoded items, and depth 8. Not every ceiling is independently reachable
after the item budget and per-record semantic limits are applied. The tests pin
reachable exact item/depth and scalar limits plus one-over rejection; 64 KiB is
an outer byte ceiling, not a claim that a valid V1 projection exists at exactly
that size.

## Local CLI

Normal `setup.sh` installation builds `o-info` and exposes it through the
installed `o info` dispatcher route. The complete V1 workflow runs on one
ordinary local machine:

```bash
o info init --state .ostadix-information
o info keygen --key .ostadix-keys/info-private.json \
    --trust .ostadix-keys/info-trust.json
o info record --state .ostadix-information \
    --key .ostadix-keys/info-private.json --pack result.info.cbor \
    --namespace local --kind research-result --coordinate name=demo \
    --predicate ostadix.local/public-scalar-v1 --scalar text --value ready \
    --acknowledge-public
o info verify --pack result.info.cbor --trust .ostadix-keys/info-trust.json
o info head --state .ostadix-information
```

`init` always derives the same empty snapshot and parentless revision. `keygen`
uses OS entropy for a mode-0600 private Ed25519 key and writes a separate trust
file containing public material only. `record` accepts exactly one public T0
scalar (`null`, `bool`, `i64`, `u64`, `f64-bits`, or `text`), appends its
declared atom locally, and writes a canonical signed CBOR delta pack. The
required `--acknowledge-public` is a caller classification, not a claim that
Ostadix can discover secrets hidden in arbitrary text.

To reintegrate a pack into another local store, initialize that store and use:

```bash
o info import --state other-information \
    --pack result.info.cbor --trust .ostadix-keys/info-trust.json
```

The importer first checks canonical encoding, object digests, signature, and
the independent public trust file. It advances `main` only when the pack base
exactly equals the current revision and the bounded entity/atom/delta/snapshot/
revision closure validates and reconstructs the unique next revision. A stale,
unsupported, or incomplete but correctly signed pack is retained byte-for-byte
under `historical-packs/` and reported as `historical-only`; it cannot advance
the head. A bad signature or untrusted signer is rejected rather than archived.

The private key is never required for `verify` or `import`, never enters a
canonical information object, and is never embedded in a pack. Every command
that could otherwise be misread as authorization prints the boundary:
information presence and signatures grant no execution authority.

## Deliberate nonclaims

Information Kernel V1 does not yet claim:

- a universal replacement for current evidence or protocol schemas;
- live distributed replication, consensus, CRDT convergence, or World
  membership;
- automatic semantic validation of every referenced native record;
- general invertibility of projections;
- permission to execute merely because a record is present or signed;
- autonomous probing with side effects. V1 value-of-information selection is
  deterministic, bounded, and read-only;
- hostile same-user resistance when an attacker can replace ancestor
  directories concurrently, or no-follow parity on non-Unix hosts;
- a transaction spanning pack publication, every content-addressed object,
  and the named-head update. Each file publication is staged and synchronized,
  while interruption between publications may leave harmless unreferenced
  objects or a separately emitted pack for later inspection.

The rollout order is shadow first, compare against existing semantics, and
only then enforce narrowly proven invariants. Existing record versions remain
unchanged until a separately reviewed compatibility boundary requires a new
version.
