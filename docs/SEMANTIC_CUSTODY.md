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
point; claims should name concrete implementations and tests rather than the
trait alone.

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

