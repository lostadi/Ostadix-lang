# Physical graph execution V1

`ostadix_api::computation` exposes a multi-operation planner and an executable
operation/transfer DAG. The implementation is in
`crates/ostadix-api/src/computation/graph_realization_plan.rs`.

## Build a request

Construct `GraphPlanningRequestV1` with:

- A validated `LogicalHGraphV2`, containing dense operation IDs and directed
  output-port to input-port edges. Its roots are exactly the terminal operations.
- One shared `ObjectiveV1`.
- A vector of `OperationPlanningRequestV1` closures, indexed by graph operation
  ID. Each closure retains the existing V1 single-operation graph and local
  operation ID zero. Its interface, contract, realization set, input geometry,
  and objective must match the corresponding global operation.
- Exactly one `TransferPlanV1` per logical edge, naming the full graph identity,
  source and destination targets, physical representations, adapter identity,
  transfer mechanism, and predicted transfer cost.

Call `plan_graph_realizations_v1(&request)`. The planner checks edge port names
and semantic types, filters operation offers against their incident transfer
coordinates, and applies the existing V1 ranking to compatible offers. The
overall predicted cost includes each selected V1 profile's uncertainty and
every transfer's estimated cost. A graph-wide objective maximum and checked
arithmetic apply to that sum.

This profile takes fixed transfer offers. To compare alternative physical
transfer routes, construct separate requests. It does not search an unbounded
Cartesian product of operation and transfer choices.

The returned `GraphRealizationPlanV1` exposes `deployment()`, `tasks()`, and
`predicted_total_ns()`. Each logical edge becomes a real transfer task whose
producer operation is its prerequisite; the consumer operation depends on
that transfer task.

## Supply executable authority

A `PhysicalAdapterRegistryV1<T>` is supplied by a trusted embedding. Register an
operation adapter at an exact realization-descriptor and target-digest pair,
and a transfer adapter at its artifact identity. Registration connects those
descriptive identities to executable capabilities. It does not authenticate a
remote target, establish artifact provenance, or discharge backend authority
by itself.

`PhysicalOperationAdapterV1<T>` implements:

- `admit`: bind the selected implementation and live target and discharge the
  required authority, state, actor, and interaction conditions.
- `execute`: consume the supplied named inputs and return every declared
  output port in a `PhysicalPortValuesV1<T>` map.

`PhysicalTransferAdapterV1<T>` implements:

- `admit`: check the concrete edge, mechanism, authority, and target bindings.
- `observe_source`: capture an immutable, typed observation identity before
  the transfer can mutate or otherwise interact with the source payload.
- `transfer`: move or convert the actual payload and return its destination
  physical value.
- `check_observation`: compare the destination with the captured observation
  under the adapter's admitted interaction and congruence contract.

Every transfer runs the observation check, including edges with identical
representations. A successful envelope/type check alone cannot publish a
consumer input. Adapter code defines the observation policy; passing that
policy is evidence about the actual execution under those admitted
interactions, not unrestricted behavioral equivalence between runtimes.

Adapters own their executable provenance checks, transport authentication,
resource lifetime, timeouts, and cancellation behavior. Payload type `T` is
generic and must implement `Clone + Send + Sync`; it may contain bytes,
`OValue`, or a native handle. No universal serialization or foreign-runtime
embedding is implied by that interface.

## Execute and observe

```rust,ignore
let plan = plan_graph_realizations_v1(&request)?;
let report = execute_graph_realizations_v1(
    &plan,
    &registry,
    &external_inputs,
    4, // maximum concurrent operation and transfer tasks
)?;
```

External inputs are keyed by `LogicalEdgeEndpointV2`; they exactly cover input
ports without a producer edge. They cannot replace a transferred input.
Before dispatch, execution recomputes the plan, checks every adapter's
admission, and validates external inputs. Values are checked against the exact
semantic type, selected physical representation, residency, and port set.

The executor refills a bounded set of worker slots on each completion. A
consumer starts only after all its transfer tasks have produced valid values
and passed their observation checks. The report contains operation outputs
and coordinator-observed Started/Succeeded/Failed transitions. A failure
suppresses new tasks, drains already-started work, and returns its observations;
it does not roll back effects already performed by an adapter.

This module is the generic embedding API. The separate
[automatic OIR bridge](OIR_PHYSICAL_EXECUTION_V1.md) derives these records from
admitted local program graphs and executes their operations and real socket
transfers through the existing coordinator. Generated native runtimes include
the records, graph API, and automatic bridge.

## Executable coverage

`crates/ostadix-api/tests/graph_realization_execution.rs` executes a three-stage
Integer -> Decimal -> Integer graph and checks consumer gating. Its process
pipe test transports both edge payloads through a separately launched
`/bin/cat`, receives sixteen actual bytes, checks both observations, and obtains
the final value 43. This is local process/pipe evidence; target authentication
continues to belong to the trusted adapters.

Other tests cover malformed edges, missing adapters and transfers, cycles,
aggregate costs, corrupted transfer payloads, wrong representations, aliased
native-handle mutation, and worker refill while an earlier task remains active.
