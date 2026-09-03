# Operation planning and observation V1

Schema/planner status: **experimental, deterministic, bounded, and
authority-free**. The marked `o run` bridge enters the pre-existing project
admission/executor path; the planner records do not grant that authority.

This surface extends the four semantic records in
[`OPERATION_REALIZATION_V1.md`](OPERATION_REALIZATION_V1.md) with physical
representation descriptions, explicit candidate tuples, descriptive cost
profiles, deterministic selection, runtime-observation records, and recovery
records. The first planner profile accepts exactly one logical operation and a
caller-supplied bounded list of complete tuples. It does not construct a
Cartesian product.

The word *selected* in this document means only that a tuple won the declared
offline objective. It does not mean admitted, authorized, reserved, placed,
dispatched, resident, reachable, or executed.

## Implemented record family

The records and their exact schema coordinates are:

| Rust record | Schema coordinate | Role |
|---|---|---|
| `PhysicalRepresentationV1` | `ostadix.physical-representation/v1` | Describes a value type, format, storage class, ownership class, and mutability. |
| `TransferPlanV1` | `ostadix.transfer-plan/v1` | Describes one adapter and representation/target crossing for one logical edge. |
| `CostProfileV1` | `ostadix.cost-profile/v1` | Carries caller-supplied aggregate costs for one exact descriptor, target, input geometry, representation tuple, and residency tuple. |
| `ObjectiveV1` | `ostadix.objective/v1` | Selects the implemented minimum-predicted-nanoseconds objective and canonical tie break. |
| `LogicalHGraphV2` | `ostadix.logical-hgraph/v2` | Names semantic operation nodes and directed value-flow edges without implementations or targets. |
| `DeploymentPlanV2` | `ostadix.deployment-plan/v2` | Records every assessed tuple, causal reasons, the selected tuple, semantic schedule, and transfer identities. |
| `RuntimeGraphV2` | `ostadix.runtime-graph/v2` | Records caller-supplied lifecycle observations and optional measurements against selected deployment candidates. |
| `RecoveryPlanV1` | `ostadix.recovery-plan/v1` | Records an ordered set of conditional alternatives for a selected candidate observed to have failed. |
| `OperationPlanningRequestV1` | `ostadix.operation-planning-request/v1` | Supplies the exact semantic closure, objective, explicit offers, and transfer records consumed by the first planner. |

The marked-project bridge also uses three separately versioned auxiliary
coordinates:

- `ostadix.project-route-pipeline/v1` identifies the versioned, deterministic
  serialized route-pipeline projection of a captured `olang.project.toml` route that a
  descriptor names as its execution pipeline;
- `ostadix.operation-runtime-binding/v1` identifies the observation binding
  between one content-verified terminal run record and the current binary's
  recomputed operation and project-route plan identities; and
- `ostadix.operation-command-error/v1` versions the JSON error envelope for the
  marked-project commands.

These are CLI bridge/report coordinates, not additional planner authority
records.

`REQUIREMENT_FOOTPRINT_CONTENT_SCHEMA_V1` has the exact value
`ostadix.placement.requirement-footprint/v1`. It labels the raw canonical
content reference used to join a frozen `RealizationDescriptorV1` to the
supplied `RequirementFootprintV1`; it is not a new authority or evidence type.

The implementation is in
`crates/ostadix-api/src/computation/realization_plan.rs` and is exported through
`ostadix_api::computation::realization_plan`. The `o-lang` compatibility package
reexports that same implementation; it does not maintain a second schema.

## Canonical bytes, identities, and bounds

Every top-level record above uses bounded canonical CBOR, denies unknown
fields, and has bounded JSON decoding for interchange. Canonical-CBOR decoding
re-encodes the validated record and rejects any byte sequence that is not its
exact canonical form. JSON may normalize set-like field order; record identity
always comes from canonical CBOR.

Each record has an independent typed identity:

```text
SHA256(record_domain || u64_be(canonical_cbor_length) || canonical_cbor)
```

The domains are record- and version-specific. Typed identities are distinct
even when two records would otherwise have identical bytes. The record limit is
4 MiB, bounded decoding uses at most 1,000,000 items and depth 128, and the
implementation separately bounds operations, edges, candidate offers, ports,
references, and explanations.

Two raw-content joins intentionally differ from typed record identities:

- a descriptor's accepted/produced representation reference is compared with
  `PhysicalRepresentationV1::semantic_ref()`, whose content is the ordinary
  SHA-256 of that representation's canonical bytes and whose schema token is
  `ostadix.physical-representation/v1`; and
- a descriptor's target-requirements reference is compared with the ordinary
  SHA-256 of the supplied placement footprint's canonical bytes under the
  `ostadix.placement.requirement-footprint/v1` schema token.

Those joins preserve the already-frozen `RealizationDescriptorV1` reference
shape. A raw semantic-content digest must not be substituted for the new
domain-separated typed record ID, or vice versa.

The OComputation manifest vocabulary adds exactly five facet kinds for this
slice: `physical_representation`, `transfer_plan`, `cost_profile`, `objective`,
and `recovery_plan`. Their `facet_ref` helpers use the manifest's raw canonical
content digest convention. The typed planning-record IDs remain distinct.

## Physical representation and residency

`PhysicalRepresentationV1` supports these descriptive storage classes:
portable value, host memory, shared memory, memory-mapped artifact, WebAssembly
linear memory, device memory, remote stream, and content-addressed artifact.
Ownership is described as owned, borrowed, shared, or streamed.

Residency is deliberately kept out of reusable physical-representation
identity. Each `PortRepresentationSelectionV1` instead carries one
`ValueResidencyV1`: portable, an exact content artifact, or an exact target
descriptor digest. This permits the same representation schema to be used with
different candidate locations and cost profiles.

These fields are descriptions. Validation does not probe memory, open an
artifact, contact a target, prove that a value is present, prove access to it,
or establish that a storage/ownership/residency combination is physically
feasible. In the single-operation profile, residency is cost and explanation
context; external-input and result movement are not automatically derived as
transfer operations.

## Logical graph and explicit offers

`LogicalHGraphV2` validates a nonempty DAG with dense graph-local operation and
edge IDs, known endpoints, no self-edge, and an exact sorted set of terminal
roots. Nodes bind exact interface, contract, realization-set, and input-geometry
identities. Nodes do not name an implementation or target.

The graph schema is general, but `OperationPlanningRequestV1` currently
requires exactly one operation. Consequently the implemented profile has no
logical edges. `TransferPlanV1` and exact edge coverage are implemented as
schema and closure plumbing, but the single-node example does not exercise
transfer choice or data movement.

Each `CandidateTupleOfferV1` supplies one complete tuple:

```text
logical operation
  x realization descriptor
  x target descriptor
  x one physical representation and residency per input/output port
  x one matching cost profile
```

Offers are bounded and canonicalized. The planner never expands independent
descriptor, target, representation, or profile lists. Supplying more than one
cost profile for the same conceptual operation/descriptor/target/port tuple is
rejected.

## Static compatibility and selection

Before ranking, the request validates the exact four-record
contract/interface/descriptor/set closure from the V1 semantic surface and
binds the graph node to it. Each offer is then assessed with machine-readable
causal reasons.

A tuple is rankable only when all of the following implemented checks pass:

- the offer names the request's sole logical operation and a descriptor in the
  exact realization set;
- the descriptor's supplied-fidelity reference exactly equals the contract's
  required-fidelity reference;
- the supplied target footprint has the exact raw content identity named by
  the descriptor and is `Complete`;
- every static footprint atom is supported by the supplied
  `TargetDescriptorV1`;
- environment, effect, and resource-minimum atoms are absent, because this
  authority-free planner cannot discharge them;
- input and output offers cover exactly the interface ports, use matching value
  types, and name representations declared by the descriptor; and
- the cost profile exactly binds the descriptor, stable realization,
  interface, contract, target digest, input geometry, and complete
  representation/residency tuple.

Passing these checks produces `StaticallyCompatibleForRanking`, not placement
eligibility. Descriptor state and actor requirements are recorded as deferred.
The pure planner in `realization_plan.rs` also does not resolve or validate the
descriptor's implementation bytes, execution pipeline, optional cost-model
document, or validation-evidence documents. The marked-project bridge performs
the narrower exact implementation and route-pipeline joins described below; it
does not change the pure planner's authority boundary.

The only implemented objective is
`MinimizePredictedTotalNanoseconds`. Its score is the checked unsigned sum of
compute, startup, conversion, transfer, queue, and checkpoint nanoseconds plus
`uncertainty_ns`. Overflow is rejected. An optional maximum is inclusive: a
tuple is rejected only when its total is greater than the maximum. Among equal
scores, the lexicographically smallest full canonical candidate tuple wins.
The objective's ruleset reference is identity-bound but its content is not
resolved or interpreted.

`DeploymentPlanV2` retains rejected and rankable assessments as well as the
winner and causal reasons. Its `schedule` is semantic vector order and must
name each deployment operation exactly once. In the current profile that
schedule contains the sole logical operation.

## Runtime observation and recovery records

`RuntimeGraphV2` can be constructed against an exact `DeploymentPlanV2` and
verified so every observation names a candidate selected by that plan. It
supports `Proposed`, `Started`, `Succeeded`, and `Failed` states. A producer may
record optional transfer, queue, startup, conversion, execution, checkpoint,
elapsed, and peak-memory measurements; observed-fidelity, failure,
actor-generation, and evidence references remain explicit optional fields.

Lifecycle validation permits a direct terminal observation or the transition
chain `Proposed -> Started -> Succeeded|Failed`; a chain may also remain at
`Proposed` or `Started`. A failed observation requires a failure-classification
reference, and non-failed observations must not carry one. These rules establish
internal record consistency. They do not authenticate the observer, evidence,
clock, target, actor, fidelity, or measurement.

`RecoveryPlanV1::verify_against` is stricter than a detached recovery record: it
requires the failed tuple to have been selected by the supplied deployment and
observed as failed in the supplied runtime graph. Every ordered alternative
must be a distinct, non-rejected candidate assessment from that same
deployment. Conditions and optional checkpoints are unresolved semantic
references.

A verified recovery record still does not retry, dispatch, restore a
checkpoint, migrate state, reserve capacity, fence effects, or grant authority.
Replanning after a successful run is not recovery and must not be represented
as a `RecoveryPlanV1`.

The marked-project CLI makes these schema boundaries concrete. `o observe`
resolves the selector to an exact run ID and reads a content-verified terminal
record and its `RunContentRefV1`. Against the current unchanged project bundle,
it recomputes the `OperationPlanningRequestV1`, `DeploymentPlanV2`, selected
`RealizationCandidateTupleV1`, project-route logical HGraph, and project-route
deployment. It requires the recorded selected route and recorded project-route
plan identities to match that current recomputation, then emits a real
`RuntimeGraphV2` containing one direct `Succeeded` or `Failed` observation. The
graph's evidence includes an `ostadix.operation-runtime-binding/v1` content
reference over those exact joins.

This is deliberately a current-binary reconstruction, not a historical
operation-plan receipt. `RunRecordV1` persisted the exact project bundle digest,
selected route, route result, and project-route logical-HGraph/deployment
identities. `RunRecordV1` did not persist the `OperationPlanningRequestV1` ID,
the operation `DeploymentPlanV2` ID, or the selected
`RealizationCandidateTupleV1`. The recomputed operation-plan IDs and tuple in an
observation therefore prove consistency with the retained record and current
bundle/current binary; they do not prove that those operation-plan coordinates
or tuple were recorded at execution time.

`o replan` always emits the recomputed `DeploymentPlanV2`; it emits a
`RecoveryPlanV1` only when that exact source observation failed and the new
selection differs. A successful source reports recovery as not applicable.
Recovery planning is not recovery execution: the command does not dispatch the
alternative or establish that it can succeed.

## Marked operation-project CLI

An existing project directory opts into this path with an `[operation]` section
in `olang.project.toml`. It names the planning-request file and provides one
explicit `(descriptor, target, cost profile)` binding for every offer. Each
binding names its realization, target label, project route, and captured
implementation file. The bundled
[`examples/normalize`](../examples/normalize) project is the executable example.

```bash
cd examples
o operation normalize
o realizations normalize
o plan normalize --explain
o run normalize
o observe normalize
o replan normalize --without-target gpu-1
```

The command boundaries are intentional:

| Command | What it does | What it does not do |
|---|---|---|
| `o operation TARGET` | Captures the exact project bundle and validates its marker, planning request, and exact implementation/cost-profile/route-pipeline bindings. | It does not rank, select, or run a tuple. |
| `o realizations TARGET` | Lists declared offers, costs, exact implementation and route-pipeline identities, route bindings, and manifest-declared unavailable targets. | It does not perform planning or live availability/eligibility discovery. |
| `o plan TARGET --explain` | Runs deterministic offline selection and emits the deployment, causal assessments, selected route, and static project route plan. | It does not dispatch, reserve, or authorize work. |
| `o run TARGET` | Repeats exact preflight, pins the selected tuple's manifest-bound project route and its project HGraph/deployment identities, executes through the existing project admission/executor path, and requires a durable run record. | Planner selection itself supplies no execution authority, and the exact joins do not prove that the route loaded the named implementation or ran on the physical target described by the tuple. |
| `o observe TARGET [--run RUN]` | Reads a content-verified terminal record, recomputes the current operation and project-route plans against the unchanged exact bundle, checks the recorded route identities, and emits a bound `RuntimeGraphV2`. The default run selector is `last-run`. | It does not execute work, recover the historical operation-plan tuple that `RunRecordV1` did not store, authenticate the observation's real-world truth, or turn a run record into trusted World state. |
| `o replan TARGET [--run RUN] --without-target TARGET_ID` | Verifies the source runtime/bundle binding, removes caller-named target offers, and emits a recomputed `DeploymentPlanV2`. It emits a descriptive `RecoveryPlanV1` only for a failed source observation when selection changes. | It does not discover unavailability, prove an independent failure domain, dispatch the replacement, perform recovery, or make a successful source run into a recovery event. |

The marked bridge has two exact descriptor joins. First, the descriptor's
implementation digest must equal the bytes of the named captured, regular,
non-symlink project file. Second, its execution-pipeline reference must equal
the deterministic serialized `ostadix.project-route-pipeline/v1` projection of the bound
manifest route,
including its route ID, kind, command, evaluator/entrypoint, working directory,
arguments, environment, prerequisites, inputs, outputs, effects, failure
continuation, result codec, provided capabilities, and guards. The manifest
binding's cost-profile digest must also match the complete candidate tuple.

Those exact joins are still declarative. They establish which artifact and
route projection the descriptor names and which route the retained project run
used. They do not prove that the route loads the separately named implementation
artifact, behavioral equivalence between realizations, or execution on the
tuple's described target.

The normalize example's `Ambient Python Primary` and `Ambient Python Fallback`
targets are descriptive static offers. Both routes execute through the ambient
local `python3`; they are not distinct observed machines or failure domains.
Selecting the alternate after excluding the primary therefore demonstrates
static replanning only, not failover independence or successful recovery.
The flagship `--without-target gpu-1` command names a manifest-declared
unavailable target for which no offer exists; it excludes zero offers, preserves
the successful selection, and correctly emits no recovery plan.

The CLI output-envelope coordinates are
`ostadix.operation-project-description/v1`,
`ostadix.operation-realization-catalog/v1`,
`ostadix.operation-plan-summary/v1`,
`ostadix.operation-observation/v1`, `ostadix.operation-replan/v1`, and
`ostadix.operation-command-error/v1`. The exact route and observation joins use
`ostadix.project-route-pipeline/v1` and
`ostadix.operation-runtime-binding/v1`, respectively. These coordinates version
reports or deterministic identity projections, not authority protocols.

## Authority and equivalence boundary

The implementation proves deterministic syntax, identity, exact referential
joins, static compatibility checks, objective evaluation, route binding, and
the internal consistency of supplied observation/recovery records. It does
**not** by itself establish:

- behavioral equivalence, refinement, numerical tolerance, or side-effect
  equivalence between realizations;
- authenticity, provenance, freshness, truth, or sufficiency of contract,
  fidelity, cost, condition, checkpoint, or validation-evidence content;
- live target health, reachability, current generation, current backend
  catalog, resource capacity, data residency, transfer feasibility, or target
  eligibility;
- distinct physical targets or independent failure domains for the normalize
  example's two ambient-Python offers;
- satisfaction of deferred state or actor requirements;
- general artifact discovery, adapter execution, implementation loading, or
  transfer. The marked bridge resolves one exact captured implementation file
  but does not prove that its bound route loads it;
- historical persistence of an operation planning-request ID, operation
  DeploymentPlan V2 ID, or selected realization tuple in `RunRecordV1`;
- admission, capability, warrant, reservation, lease, dispatch, retry,
  recovery execution, checkpoint restore, effect fencing, settlement, or World
  authority; or
- general multi-operation planning. The current planner profile is one logical
  operation even though the graph and plan records reserve a wider shape.

Those boundaries are not temporary implications hidden behind a successful
`validate`, `plan`, `verify_against`, or CLI exit status. A later authority or
execution layer must consume the exact records and establish its own fresh
preconditions.
