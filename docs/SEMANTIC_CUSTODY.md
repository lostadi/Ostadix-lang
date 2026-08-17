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

V1 is shadow-only. Catalog V5 hashes the explicit optional profile assignment
and resolves it through canonical backend names and aliases; archival V4
identity remains unchanged. The profile is not a `BackendInterface` field and
does not itself authorize evidence, admission, placement, or dispatch. Existing
current-catalog projections bind the Catalog V5 digest without adding a new
evidence field. Package 0.3's current Graph V2/Evidence V6 path preserves the
complete typed solver assessment in its hashes and Why V2 view and is accepted
by the coordinator, but still does not enforce the catalog profile. Graph
V1/Evidence and Admission V5 remain archival; there is no conversion from V5
to V6. The
shadow result can therefore expose differences such as the compatibility
solver's optimistic container classification without reducing current
execution capacity.

## Executable artifact

After building `O` and `olangc`, run:

```bash
bash scripts/semantic_custody_demo.sh
```

The ignored output directory `target/semantic-custody/` contains:

- `execution-intent.json` — stable, authority-free semantic identity;
- `schedule.txt` — fresh admission and static schedule explanation;
- `hgraph.dot` — graph inspection view;
- `result.json` — observed result from a newly admitted same-intent-gated run;
- `manifest.json` — hashes, bounded claims, and explicit nonclaims.

The demonstration intentionally does not manufacture a signed Hosted V2 or
World receipt. Those require their own authority, transport, and lifecycle
contracts.

## Explicit nonclaims

- A schedule wave is not proof of simultaneous physical execution.
- A matching intent is not permission to execute.
- A local terminal result is not a signed remote receipt.
- `OValue` is not synonymous with `PortableOValue`.
- Hosted V2 currently executes prepared hosted fragments; it is not general
  graph migration or transparent fallback.
- O-core/native evidence is a related compiler/runtime chain, not evidence that
  every hosted operation passed through O-core.
