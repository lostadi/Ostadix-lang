# Bounded semantic custody

Ostadix models heterogeneous execution as explicit transformations rather than
opaque process edges. The current implementation contains several linked
custody chains; it does not claim one universal source-to-World proof.

```text
exact .O source bytes
        |
        v
parsed expressions -> OIR -> execution plan -> solved HGraph
        |                                      |
        |                                      v
        |                            stable execution intent
        |                            (sameness, no authority)
        v                                      |
runtime/effect/backend evidence ---------------+
        |
        v
fresh AdmittedExecution -> dispatch -> observed local result

separately, when Hosted V2 is selected:
signed placement evidence -> durable session journal -> signed node receipts
```

## What each boundary establishes

- **Source and execution intent:** binds exact source bytes to OIR, plan,
  analyzed HGraph, catalog projection, analyzer identity, and base policy.
- **Admission:** binds live runtime discovery, backend artifacts, environment,
  policy, and evidence to the exact executable graph accepted for dispatch.
- **OValue crossing:** represents hosted values in process. A value being an
  `OValue` does not by itself make it portable, replay-safe, or admissible.
- **PortableOValue:** allowlists the values permitted across the bounded World
  value boundary.
- **Hosted V2 journal:** records authenticated session mutations and signed
  node receipts under the exact durable protocol. It is not a general World
  ledger or automatic migration service.
- **Settlement:** records what the selected runtime observed. Proposal,
  admission, dispatch, and observation remain distinct events.

## Fidelity boundary

Fidelity assessments distinguish definite loss from possible loss and compose
those bounds across modeled paths. They do not yet prove every backend
crossing: render fidelity, capability transfer, wire serialization, and World
portability are separate checks. `BackendMorphism` is a law-bearing extension
point; its unversioned declaration alone establishes no backend claim.

The implemented relationship is specifically between concrete `Fidelity` and
`FidelityAssessmentV2`. `from_concrete` is an exact point embedding: a concrete
structural loss set `L` becomes the interval `[L, L]`, and the three
non-structural cases remain singleton points. For concrete judgments `a` and
`b`, generated properties establish
`from_concrete(a.compose(b)) == from_concrete(a).then(from_concrete(b))`.
`concretization_contains` implements membership in the corresponding interval
without materializing its potentially exponential powerset, and generated
witnesses establish that `then` conservatively contains concrete sequential
composition. The compatibility upper projection is also compositional and is
a left inverse on point embeddings.

Structural intervals created through the checked constructor or wire decoder
enforce `definite` as a subset of `possible`; serialization rejects an invalid
interval assembled directly through the existing public enum fields.
`join_paths` is a tested, conservative all-observable merge, but no production
solver path currently calls it. Its concretization-as-hull claim is restricted
to lossless/structural intervals: `NativeCapsule` and `Unsupported` are
absorbing severity classes rather than a representation of cross-class
disjunction.

`RenderFidelity` is separate. It classifies a source-splice renderer as typed,
structural, presentational, or opaque and can be recomputed descriptively on
demand; it is not currently recorded or enforced by admission. Some
typed/structural distinctions are payload-conditional for Python Decimal/F64
and Nix integer/text/map values, and recursive containers fold child
classifications within one renderer. No implemented conversion or Galois
connection relates it to `Fidelity` or `FidelityAssessmentV2`.

`BackendMorphismV1` adds a bounded semantic kernel for the current Python,
JavaScript, and Rust adapters. It reports O-to-backend-input and profiled
backend-output-to-O fidelity as distinct legs, composes them, checks the
lossless round-trip law, and returns typed rejection for cycles/shared
references, non-string or duplicate map keys, excessive nesting, unsupported
values, and numeric/profile limits. The method named `inject` consumes an
already acquired backend-output value; for JavaScript and Rust, recursive
containers on that leg mean JSON/stdout egress only and do not imply a generic
input-binding channel. The Python profile covers recursive acyclic plain data.
JavaScript admits only scalar native inputs; Rust covers bounded scalar source
constants and profiled stdout, not arbitrary Rust programs or bindings.
Executable conformance tests exercise a nested OValue binding into Python, a
cross-runtime scalar binding into JavaScript, and the exact Rust program emitted
from each projected scalar before it is compiled and run by the real shim.
They also cover negative boundaries. Richer runtime values outside these
profiles remain executable but acquire no V1 morphism claim.

V1 is shadow-only. Catalog V5 introduced the hashed optional profile assignment,
and Catalog V6 retains it and resolves it through canonical backend names and aliases; archival V4
identity remains unchanged. The profile is not a `BackendInterface` field and
does not itself authorize evidence, admission, placement, or dispatch. Existing
current-catalog projections bind the Catalog V6 digest without adding a new
evidence field. Package 0.3's current Graph V2/Evidence V6 path preserves the
complete typed solver assessment in its hashes and Why V2 view and is accepted
by the coordinator, but still does not enforce the catalog profile. Graph
V1/Evidence and Admission V5 remain archival; there is no conversion from V5
to V6. The
shadow result can therefore expose differences such as the compatibility
solver's optimistic container classification without reducing current
execution capacity.

## Operation-description boundary

Operation-realization V1 adds separate authority-free descriptive records.
Their construction and reading progression is:

```text
OperationContractV1
        -> OperationInterfaceV1
        -> RealizationDescriptorV1...
        -> RealizationSetV1
```

Those arrows are progression, not stored-reference direction. The actual
checked back-references are:

```text
OperationInterfaceV1      -> OperationContractV1
RealizationDescriptorV1   -> OperationInterfaceV1
RealizationDescriptorV1   -> OperationContractV1
RealizationSetV1          -> OperationInterfaceV1
RealizationSetV1          -> OperationContractV1
RealizationSetV1          -> RealizationDescriptorIdV1...
```

Each record has bounded canonical-CBOR bytes and an independently
domain-separated typed record identity. An `OComputationManifestV1` facet uses
the ordinary SHA-256 of those canonical bytes as its `content` identity; that
raw facet digest is intentionally distinct from the typed record identity.
`o operation inspect` validates one record while leaving its references
unresolved. `o operation verify` requires one exact supplied closure and checks
only interface-to-contract, descriptor-to-interface/contract, descriptor-port,
and set-to-interface/contract/descriptor consistency. Empty validation evidence
means declaration-only; nonempty evidence references are not resolved or
authenticated.

An `OComputationManifestV1` may name these bytes with the corresponding facet
kinds and explicit derivations. Merely decoding a semantic record does not add
it to a computation lineage, establish how it was derived, or make its claims
true. Referential consistency is not planning, realization selection,
behavioral equivalence, evidence authenticity, target eligibility, placement,
execution, recovery, admission, capability, lease, or World authority. See
`docs/OPERATION_REALIZATION_V1.md` for the complete V1 boundary.

## Executable artifact

After building `O` and `olangc`, run:

```bash
bash scripts/semantic_custody_demo.sh
```

The ignored output directory `target/semantic-custody/` contains:

- `execution-intent.json` — stable, authority-free semantic identity;
- `schedule.txt` — inspection-only admission and static schedule explanation;
- `hgraph.dot` — rendered graph inspection view, not the solved HGraph record;
- `result.json` — observed result from a newly admitted same-intent-gated run;
- `computation.cbor` — canonical `ostadix.ocomputation-manifest/v1` body;
- `computation.json` — the same computation manifest projected as JSON;
- `manifest.json` — V2 publication envelope with hashes, the computation
  revision, bounded claims, and explicit nonclaims.

The computation manifest hashes the exact source and four generated artifacts.
It also records the exact `O` and `olangc` executable byte identities as root
facets and workflow-attested transformer identities; the executables themselves
are not copied into the output directory. The schedule and DOT facets remain
explanatory and rendered views. In particular, the observed result edge names
the source, required execution intent, and exact `O` binary identity, never the
schedule explanation.

The derivation edges attest what one locked staged shell workflow invoked. They
are unsigned provenance claims, not cryptographic proof that those historical
processes executed. Canonical decoding verifies the named content identities
and derivation-graph structure. The schedule bytes bind their V6 shim, runtime,
environment, and ambient-world digests, but those inputs, the Python runtime,
and historical process identity are not separate rooted facets. The per-output
lock prevents cooperating demo invocations from interleaving publication. The
shell publishes all six hashed artifacts before it publishes the outer
`manifest.json` last.

The demonstration intentionally does not manufacture a signed Hosted V2 or
World receipt. Decoding either computation representation also reconstructs no
admission, placement, dispatch, or reusable runtime authority. Those require
their own authority, transport, and lifecycle contracts.

## Explicit nonclaims

- A schedule wave is not proof of simultaneous physical execution.
- A matching intent is not permission to execute.
- A local terminal result is not a signed remote receipt.
- `OValue` is not synonymous with `PortableOValue`.
- Hosted V2 currently executes prepared hosted fragments; it is not general
  graph migration or transparent fallback.
- Operation-realization V1 records and `o operation inspect|verify` establish
  descriptive identity and referential consistency only; they do not plan,
  select, place, execute, recover, prove equivalence, authenticate evidence, or
  grant authority.
- O-core/native evidence is a related compiler/runtime chain, not evidence that
  every hosted operation passed through O-core.
