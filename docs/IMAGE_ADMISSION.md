# Image admission and codomain slack

Ostadix uses two different shapes of validation, and audits must not confuse
them. A predicate can select acceptable values from a declared type. A trusted
analyzer can instead determine which values are reachable at an authority
boundary. The first creates a checker-coverage obligation. The second creates
an analyzer-image obligation.

Ostadix Evidence V5 and V6 use a third, deliberately qualified form: the
trusted analyzer determines every hard execution fact, while a soft cost
estimate may differ from the analyzer result without changing execution
legality. The precise architectural claim is therefore **hard-projection image
admission**, not whole-record image admission.

## Three admission forms

Let `T` be a declared record type.

### Predicate admission

A predicate-admitting boundary defines its trusted set with a checker

```text
C : T -> Bool
Trusted = C^-1(true)
```

The caller can present values from `T`; the checker decides which values enter.
The audit question is whether `C` handles every reachable form of `T` and
checks every field on which the protected property depends. A new enum variant
or field can create a new obligation even when no current producer constructs
it, because external construction or deserialization may still reach the
checker.

Normalization is predicate admission. Reconstructing a record from its own
fields and comparing it with the input proves canonical form, not independent
derivation of the claim.

### Full image admission

An image-admitting boundary has a trusted derivation

```text
A : S -> T
Trusted = im(A)
```

where `S` is the independently bound source/context space. An implementation
may accept a caller-supplied candidate `t`, but only by recomputing `A(s)` and
requiring whole-record equality `t = A(s)`. Downstream code receives the result
only through the admitted authority handle.

The audit question is now what `A` can produce. Values in `T` that are outside
`im(A)` cannot reach the protected consumer through this boundary. A wider
declared codomain is therefore compatible with a narrower current analyzer
image.

### Projected image admission

Many useful records mix authoritative and descriptive fields. Let

```text
pi : T -> H
```

be the explicit projection onto hard fields. A projected-image boundary
requires

```text
pi(t) = pi(A(s))
```

while the remaining soft fields stay outside the image theorem and retain
their own, explicitly stated acceptance and consumer contract. The trusted
hard set is `im(pi . A)`. The complete accepted record set need not be `im(A)`,
because multiple soft values may occupy one hard-field fibre.

Projected image admission licenses codomain slack only for the hard
projection. Any property claimed invariant under soft-field changes must be
shown to factor through `pi`:

```text
legal = legal_h . pi
```

This is the exact form used by current Ostadix execution admission.

## Four audit preconditions

The image argument is valid only when all four conditions hold.

1. **Trusted, deterministic derivation.** `A` is total on the stated
   admissible source space and deterministic there. Repeating analysis for the
   same bound source/context yields the same hard projection. Analyzer failure
   rejects admission; it does not create an unchecked value.
2. **Explicit equality scope.** Admission recomputes `A(s)` and compares either
   the whole record or a named projection `pi`. Every excluded field is
   documented as soft and retains its own acceptance, provenance, and consumer
   obligations; it must not silently influence the protected hard property.
3. **Source and context binding.** The source analyzed by `A` is digest-bound
   to the source admitted and consumed. Source, plan, graph, analyzer, runtime,
   environment, and other relevant context cannot be mixed between analysis
   and use.
4. **Consumer confinement.** Protected consumers obtain the record only
   through an opaque admitted handle. A raw record, graph, or deserialized enum
   cannot bypass the boundary and be interpreted as admitted authority.

These preconditions are also the audit checklist for applying the theorem at a
new boundary.

## Codomain Slack Theorem

**Theorem.** Suppose a boundary satisfies the four preconditions for
`A : S -> T`, and let `Reach` be the set of records that its protected consumer
can receive as authority. Under full image admission,

```text
Reach subseteq im(A).
```

Consequently, every value in `T \ im(A)` is unreachable through that authority
boundary. Extending `T` without changing `A`, admission equality, or the
consumer boundary does not extend authorized behavior.

Under projected image admission,

```text
pi(Reach) subseteq im(pi . A).
```

Consequently, hard values outside `im(pi . A)` are unreachable. Soft values
may vary inside a hard-field fibre, and only properties proven to factor
through `pi` are invariant under that variation.

**Proof.** By the equality condition, every admitted whole record equals
`A(s)`, or every admitted hard projection equals `pi(A(s))`, for the bound
source `s`. Source/context binding prevents substituting a different source
after analysis. Consumer confinement prevents introducing a raw value through
another constructor or decoder. Therefore every authoritative consumer input
is in the stated image. The projected conclusion follows by applying `pi`.
For a property that factors through `pi`, equal hard projections give equal
property values. QED.

This is a reachability theorem, not a kernel-membership theorem. For a function
`f : T -> U`, `ker(f)` is normally an equivalence relation on pairs of elements
of `T`; an expression such as `T \ im(A) subseteq ker(f)` is ill-typed. If an
observational statement is needed, state it with two reachable records:

```text
pi(t1) = pi(t2) implies legal(t1) = legal(t2)
```

and prove that `legal` factors through `pi`.

### Corollary: safe declared slack

A declared hard-field codomain may be strictly wider than the analyzer's image
without an attestation obligation over the difference. The unused values are a
versioning affordance until the analyzer, equality projection, or consumer
boundary changes.

This does not make direct consumers total over the larger type, and it does not
license wildcard handling as compile-time exhaustive. Explicit enum arms can
still be useful so that extending the enum forces a compiler review.

## Ostadix Evidence V5 and V6

Current execution admission realizes the projected theorem.

### A1: trusted derivation

The trusted analyzers construct node evidence from the solved executable graph
and runtime binding in
[`evidence/analyze.rs`](../crates/ostadix-api/src/evidence/analyze.rs). Their
current dispatch-lane image is exactly `LocalWorker` or `Coordinator`; the
choice is derived from the classified worker candidate rather than accepted as
a caller label.

V5 and V6 admission independently rerun those analyzers before accepting a
bundle:

- V5 recomputation begins in
  [`admit.rs`](../crates/ostadix-api/src/evidence/admit.rs) at
  `admit_execution_v5`;
- V6 recomputation begins at `admit_execution_v6` in the same file.

### A2: hard-projection equality

For every operation, both versions require equality with the recomputed
baseline for these hard fields:

- type contract;
- effect contract;
- dispatch contract;
- capability disposition and provenance;
- placement and provenance;
- failure contract;
- resource demand.

The comparisons are in `validate_node_evidence_v1` and
`validate_node_evidence_v2` in
[`evidence/admit.rs`](../crates/ostadix-api/src/evidence/admit.rs).

`cost_estimate` is intentionally absent from that equality. It is soft
evidence, not a blocker, lane, wave, capability, or legality input. The test
`soft_cost_estimates_cannot_change_legal_blockers_or_waves` proves that changed
historical costs remain admissible and change the evidence digest while the
legal projection remains identical. Ostadix therefore does **not** claim
whole-record equality or full image admission for `NodeEvidenceV1` or
`NodeEvidenceV2`.

In symbols, with `H` the hard projection,

```text
admitted(e, s) implies H(e) = H(analyze(s)).
```

### A3: source and runtime binding

Admission checks the exact evidence bindings before reanalysis. The bindings
cover the lowered OIR, canonical plan, analyzed graph, backend-catalog
projection, backend set and executable manifest, launch context, environment,
ambient World, and analyzer identity. Their construction is in
[`evidence/analyze.rs`](../crates/ostadix-api/src/evidence/analyze.rs), and both
admission versions reject a mismatch before comparing node evidence.

### A4: consumer confinement

The current coordinator constructor accepts
`AdmittedExecution`, not a raw HGraph or evidence bundle. See
[`executor/coordinator.rs`](../crates/ostadix-api/src/executor/coordinator.rs).
That opaque authority type is the protected path to dispatch.

### Dispatch-lane consequence

Six declared `DispatchLaneV1` values are currently outside the analyzer's hard
image. They are unreachable by the coordinator through fresh V6 admission even
though the coordinator is not designed as a general executor for all eight
declared lanes. This is benign codomain slack at the current authority
boundary, not evidence that every direct use of the enum would be safe.

Changing the analyzer to construct another lane, excluding dispatch from the
hard projection, or admitting raw evidence to the coordinator would invalidate
the corollary and require a new review.

## Connection to contextual provenance recovery

Provenance V2 should preserve the distinction between structural consistency
and admitted epistemic authority.

A raw provenance claim may carry a witness whose discriminant determines an
origin such as declared, derived, observed, predicted, or counterfactual. Making
`origin()` a method prevents a stored label from disagreeing with that witness,
but it does not prove that a measurement occurred, that a derivation ran, or
that an enforcement mechanism was active. If the witness is caller-supplied
and deserializable, `validate_shape()` remains predicate admission.

Full provenance authority requires a contextual analyzer

```text
R : (raw claim, trusted receipts, trust policy, recovery question) -> admitted provenance
```

that resolves and verifies the referenced observation, derivation,
enforcement, or execution evidence. It should return an opaque admitted handle;
protected consumers must not accept the raw claim as equivalent. The recovery
question must state what is being recovered, the observation equivalence, and
the counterfactual domain. Exactness is therefore relative to a context and a
question, not a zero-argument property of a witness enum.

For a derivation, an addressed procedure and listed inputs can create residual
obligations. A source- and output-bound execution receipt may discharge those
obligations. Missing execution fidelity is an unresolved obligation, not a
permanent loss: the current `LossContractV1::sequence` only accumulates losses
and cannot later remove one. Provenance recovery should consequently keep
intrinsic loss, residual obligations, and their typed discharges separate.

The first execution adapter discharges cryptographic receipt-signature
verification, not signer authorization. `VerifiedExecutionReceiptV1` proves
the signature against a caller-supplied resolver but retains no trust-policy
identity, so producer authentication, signer authorization, and receipt
currentness remain explicit obligations beside execution and morphism fidelity.

The resulting architecture has two nested image boundaries:

1. the provenance analyzer image-admits its origin classification, assurance
   statements, and recovery judgment relative to bound context; the opaque
   handle exposes an established origin only when that recovery is `Exact`;
   and
2. execution admission image-admits the hard evidence projection used to
   authorize any Ostadix execution cited as a provenance discharge.

Neither a content address nor an information-snapshot membership claim becomes
execution authority by itself.

## Paper-sized statement

> Predicate admission trusts the preimage of a checker. Image admission trusts
> the output of a bound derivation. Ostadix execution admission is the projected
> form: trusted reanalysis fixes every hard execution fact, while auditable soft
> costs may vary without changing legality. The Codomain Slack Theorem licenses
> declared hard-field values outside the analyzer image because they are
> unreachable, not because they belong to a consumer kernel.
