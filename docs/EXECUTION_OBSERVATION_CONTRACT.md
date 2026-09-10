# Execution observations and scheduling correctness

Ostadix defines execution through transitions between configurations. A
configuration includes pending operations, materialized values, environments,
World state, and outcome. A transition can emit an event or be internal. An
execution is a finite or infinite path; a specified observation policy maps
that path to retained behavior. Reusable component replacement additionally
quantifies over explicitly admitted surrounding interactions.

## Runtime capability scope

The executable interfaces below implement bounded parts of the six runtime
requirements. They do not establish the unrestricted forms of those requirements.

| Requirement | Executable support | Remaining boundary |
| --- | --- | --- |
| Lossless native-object exchange | Python handles retain arbitrary objects and identity; O, Python, and the Unix Node bridge invoke, read, write, and release them through their exact admitted owner. | Execution stays in the live Python owner. Other native owner runtimes, arbitrary reconstruction, and migration of live handles need additional adapters. |
| Authoritative morphisms on edges | Opt-in Python plain-data crossings validate actual inputs and outputs; the physical graph executor requires an observation check on every transfer. | Each contract has a defined carrier and trusted adapter. This is not universal enforcement of contextual equivalence across all backends. |
| Parallel evaluator blocks | Explicit autonomous groups admit fresh backend bodies with materialized nested evaluator inputs and refill worker slots as dependencies complete. | Persistent actors, callbacks, policy boundaries, and failure settlement retain their applicable coordination rules. Arbitrary effects are not proven serial-equivalent. |
| Multi-operation physical planning | The embedding API executes operation/transfer DAGs; the automatic OIR bridge derives complete admitted local operation graphs and moves actual inputs through checked socket transfers. | Automatic remote placement and authenticated remote transfer adapters are separate work; the automatic profile remains local. |
| Recovery and migration | Local handoffs and authenticated Hosted V2 checkpoint transfers restore supported actor state, require acknowledgements, durably fence the source, and recover interrupted handoff phases. | Quiescent Python/SQLite codecs are supported. In-flight computation, live handles, external resources, and automatic failover without signed fencing remain outside this handoff. |
| One-binary foreign runtimes | Native compilation embeds runtime payloads. The Linux rootfs profile collects declared ELF dependencies/data and executes inside private namespaces with an immutable image and private network. | Each runtime closure still needs qualification. Host kernel/initial loader and standard streams remain inputs; computed imports, external services, and every catalog runtime are not automatically supplied. |

Interface details and executable coverage are documented in
[native handles](PYTHON_NATIVE_HANDLES.md),
[crossing enforcement](BACKEND_MORPHISM_ENFORCEMENT_V1.md),
[physical graphs](PHYSICAL_GRAPH_EXECUTION_V1.md),
[local actor migration](LOCAL_ACTOR_MIGRATION.md),
[Hosted actor migration](HOSTED_ACTOR_MIGRATION.md), and
[runtime bundles](EMBEDDED_RUNTIME_BUNDLES.md).

## Regression observation policy

For the terminating examples in `tests/executor_state_complete.rs`, the
observation is process termination, stdout bytes, normalized stderr, and
selected relative file paths with their bytes. Termination compares the full
`ExitStatus`, preserving distinct Unix signals. The file projection excludes
permissions, timestamps, empty directories, and external state. The suite
compares selected completed executions, not every trace or every context.
[Rust's ExitStatus contract](https://doc.rust-lang.org/std/process/struct.ExitStatus.html)
explains why comparing only `code()` loses signal distinctions.

## Finite DAG theorem

Fix a finite acyclic operation graph, deterministic terminating atomic
transitions `T_v: W -> W`, an equivalence `R` on `W`, and final observation `o`.
Require:

1. Every `T_v` preserves `R`.
2. `w R w'` implies `o(w) = o(w')`.
3. Every pair incomparable in the graph commutes modulo `R` on relevant
   reachable states: `T_u(T_v(w)) R T_v(T_u(w))`.

Then all complete topological orders have equal observations. Any two such
orders are connected by swaps of adjacent incomparable operations. Condition
3 relates the states after a swap; condition 1 carries the relation through
the suffix; condition 2 equates final observations. Serial equivalence also
requires the reference serial order to be a permitted complete order.

`hgraph::semantics::check_finite_confluence` checks these hypotheses against
the actual HGraph input-producer dependencies and supplied finite transition
tables. It requires total tables for exactly the executable operations and
conjunctive readiness. It checks all model states, which is stronger than
checking reachable states. It is bounded to 256 states and 256 operations.
Its result is descriptive; it grants no admission and does not establish that
a foreign adapter implements the tables. The tests enumerate all six orders
of an independent three-operation model, reject a suffix that distinguishes
equivalent states, and require ordering between noncommuting write/fail steps.

Unknown effects, callbacks, persistent actors, failure, and internal operation
interleavings belong in the model before that theorem can justify reordering.
Explicit `autonomous(...)` opts into unordered effects; it is not evidence
that those effects commute with the serial reference. Worker preparation can
consume materialized nested evaluator inputs while retaining child data and
completion dependencies. Stateful actors and inner `lazy(...)` boundaries
keep their existing ordering obligations.

## Equivalence records what coincides, not what it means

For transition monoid `M = <F>` including identity, the future-stable part of
an equivalence is

```text
i_F(epsilon) = intersection over m in M of (m x m)^-1(epsilon)
theta_o = i_F(kernel(o))
```

With inclusion `j: Con(A) -> Eq(X)`, `j` is left adjoint to `i_F`. Observations
can be ordered by kernel inclusion, identifying observations with equal
kernels. Quotient-class observations can represent every congruence; choosing
such an observation already presupposes the desired quotient.

The congruence lattice and even the entire family of these partitions do not
determine behavior values. On `{0,1,2}`, identity and constant-zero operations
preserve every equivalence, but state `1` has traces `1,1,1,...` and
`1,0,0,...` under identity observation. The regression model retains this
counterexample. For a fixed carrier the representation criterion is
`L = Con(End(L))`; a complete sublattice alone does not suffice. See §2.2 of
[Jakubikova-Studenovska, Poschel, and Radeleczki](https://web.uni-miskolc.hu/~matradi/files/2017JPR_forAUfinal_forPublication1.pdf).

For deterministic total transitions the behavior map into `O^(D*)`, equipped
with shifts, is a homomorphism and `A/theta_o` is isomorphic to its image.
The image need not be the full behavior space. An OValue lifting is adequate
for a declared interaction family only if every permitted future interaction
is determined by the lifted value. Equal OValues cannot erase environments
that an admitted callback can distinguish. `Set/Beh` has terminal object
`(Beh,id)`; changing categories alone does not prove this lifting obligation.

Extending observations gives
`theta_(o1,o2) = theta_o1 intersection theta_o2`, a non-strict refinement in
general. Adding interactions changes the transition family and can refine
equivalence separately. A rule without observation/specification input
cannot recover all independently varying contracts: constant and injective
observations already force different quotients on any nontrivial carrier.

## Lowering termination and fidelity convergence

A multiset rank can justify graph lowering when every rewrite removes nodes,
creates only lower-ranked nodes, and retains operand subgraphs by reference.
Duplicating a substituted higher-ranked subterm invalidates that argument;
every actual rewrite needs a decrease or another well-founded component.
Well-foundedness excludes infinite descent, not ascent. The implementation
does not yet assign and verify such ranks for every lowering rule. See
[Dershowitz and Manna](https://www.cs.tau.ac.il/~nachumd/papers/LNCS/Multisets.pdf).

The type solver's finite convergence budget counts possible lattice changes.
Its generated loss labels and their budget contribution now derive from the
same closed vocabulary. Existing labels in incoming facts are also counted.
Domain intersections and accumulating fidelity unions impose monotonicity;
debug assertions check those directions and are not their sole enforcement.
Conflicting materialized values remain runtime errors. This solver argument
is separate from termination of arbitrary evaluator code.

State simulations can connect these transitions to execution observations,
including source steps represented by several target steps. Divergence
preservation also needs control of infinite internal stuttering; see
[Leroy's verified compiler backend](https://xavierleroy.org/publi/compcert-backend.pdf).
