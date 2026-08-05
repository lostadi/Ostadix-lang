# State-complete HGraph executor

The Rust hosted runtime executes OIR through a directed HGraph. The graph
coordinator is the default; `O_EXECUTOR=serial` selects the reference OIR
executor used by the differential conformance suite.

Project inputs have a distinct logical-planning and opt-in hosted execution
surface:

```text
ProjectBundle -> shared ResolvedSelection -> ProjectExecutionPlan
              -> ProjectHGraph -> ReadySchedule -> ProjectCoordinator
              -> OExecutionResult + ProjectAttemptTrace
```

Routes are not synthesized as fake OIR. The project planner binds its source to
the exact deterministic bundle digest and normalized route policy, constructs
one logical materialization branch per selected alternative, recursively
places prerequisite routes inside that branch, and then projects real project
operation kinds. Its project-specific validator reconstructs the canonical
source plan and checks the exact operations, dependencies, effects, values, and
graph projection. This closes a provenance gap that generic HGraph
well-formedness and the intentionally OIR-only `source_plan` field cannot close.

`olangc <project> --target ir|dot` remains inspection only. With
`O_PROJECT_EXECUTOR=hgraph`, project script execution and compiled project
binaries use `ProjectCoordinator` for one resolved `Explicit`/`Default`
alternative or serial ordered `Fallback`/`AnySuccess` alternatives. The graph
controls isolated materialization, prerequisite readiness, route execution,
and policy selection. `Fallback` uses resolved priority order; `AnySuccess`
uses declaration order. Both preserve the attempted result prefix and stop
before materializing a later alternative after the first successful result.
Parallel/racing and aggregate policies fail closed and never fall back to
`run_selection`.

The compatibility project runtime remains the default when the environment
variable is unset. In either mode, materialization and commands remain
fallible `HostWorld` work even if a manifest declares `pure=true`. Logical
alternative branches still share conservative ambient/resource chains; the
ordered executor is not evidence of parallel or independently mediated branch
execution.

The normative World-v3 constitution and Hosted World profile are byte-sealed
by the append-only G0 evidence ledger. Their older repository-status paragraphs
are not rewritten by this hosted executor patch; changing those bytes requires
a separate constitution-source refresh, a new schema-v3 G0 attestation, and an
explicit supersession event. Nothing here claims G1.

Project dependencies distinguish `Value(pN)` from `Success(pN)`. A settled
nonzero route publishes its ordinary result and conservative resource
successors, but not its successful-completion token. This lets selection inspect
a terminal unsuccessful result while preventing a failed prerequisite from
activating its dependent route. Guard skips preserve the compatibility
runtime's successful progression semantics. Infrastructure aborts publish no
route result, completion, or resource successor.

## Implemented operation shape

```text
ordinary child/data values ----\
completion dependencies --------> [ Execute(plan node) ] -> ordinary OValue
resource state R@n ------------/                         -> Completion(plan node)
actor state A@n --------------/                          -> resource state R@n+1
                                                         -> actor state A@n+1
```

Every executable hyperedge has one distinguished ordinary result and one
successful-completion output. Resource/control outputs are synthetic scheduling
values and carry no `OValue`. Every output has exactly one producer.

## Effect and state model

`src/effects.rs` derives summaries before executable edges are built. The
currently modeled resource keys are:

- `HostWorld`
- evaluator-local state
- O scope bindings
- project-relative paths
- host paths
- environment variables
- standard I/O
- exact or unknown network endpoints
- named services
- persistent actor state keyed by canonical language and environment number
- exact governed World/namespace epochs and descriptive Governor positions
- exact node, domain, and process generations
- owner-scoped generic resource, device, and accelerator generations
- object versions, descriptive capability identities, task attempts, and
  artifact publication state

Unknown hosted operations read and write `HostWorld` and evaluator-local state.
`HostWorld` aliases precise host resource declarations conservatively. A
persistent shim also consumes and produces its actor-state token. The live
process registry does not expose a trustworthy generation, so actor resource
identity does not invent a constant generation field.

Governed resource keys do not alias `HostWorld`: they are vocabulary intended
for a future trusted World/O-core lowering. Device and accelerator views do
also expand to the canonical generic governed-resource key so the same resource
cannot bypass a dependency through a different typed view. A key by itself is
not proof of mediation or authority, and `CapabilityState` is descriptive
identity rather than a grant. Source `reads=` and `writes=` declarations cannot
construct these keys, no production lowering emits them yet, and today's
arbitrary hosted backends keep their conservative `HostWorld` dependency.
`olangc file.O --target ir --grounding` renders the distinction,
capability-right requirements, actor and
capsule affinity information, and any residual ambient dependency. Optional
`--world-id NAME --world-epoch N` binds that inspection report to an exact
caller-supplied epoch; it does not consult a live snapshot, enforce freshness,
perform placement, or execute the plan.

Effect attributes are checked constraints. `effects=unknown` can downgrade a
verified renderer. `effects=pure` cannot upgrade an arbitrary shim. `reads=`,
`writes=`, and `serial=host` add dependencies without removing unknown
fallbacks. Backend authority remains a permission model and is not treated as
an exact footprint.

## Sequence and group control

Ordinary source sequence is implemented by adding the earlier operation's
completion token to the later operation's executable inputs. Sequence is
relaxed only in either of these cases:

1. Both operations are direct members of the same explicit `batch`, `all`,
   `any`, or `race` group.
2. Both operations are verified, deterministic, infallible, resource-free
   inline renderers from the trusted `html`, `markdown`, `text`, and `latex`
   set; both complete structural subtrees contain only literal text and
   recursively trusted renderers; and neither operation is a child of a
   structural `O` sequencing region.

If any fact is unknown, the completion dependency remains. Explicit group
topology does not override resource conflicts: unknown members still share the
directed `HostWorld` chain.

## Scheduling and failure

`ReadySchedule` builds a producer map over every ordinary and synthetic output.
An operation's blockers are exactly the producers of its input nodes. There is
no separately maintained actor or effect blocker in the final scheduler. The
one explicit exception to conjunctive input readiness is recorded in
`ReadyOp::input_policy`: `SelectRoute(fallback|any_success)` derives
`OrderedFirstSuccess`. Its ordered inputs are alternative results, so the
operation can settle after the first successful prefix member or after all
members settle unsuccessfully. Every other operation derives `All`; the
project coordinator rejects a policy/operation mismatch rather than silently
bypassing readiness.

The ordinary OIR coordinator owns the mutable evaluator and process registry.
It launches only verified pure inline renderer tasks on worker threads. After
each success, it materializes the value, completion, and successor-state
outputs and derives a fresh frontier. On failure, it emits none of those
outputs and admits no later dependent operation. Root values and scope writes
commit in deterministic source order, but commit order is not used to justify
early side effects.

The separate hosted `ProjectCoordinator` is serial and uses the conservative
launch rank plus stable ordinal as its baseline/tie-break order. For ordered
first-success policies it prioritizes the now-ready `SelectRoute` choice over
later potential branches; those later operations never receive `Ready` or
`Started` attempt events. A valid nonzero result or guard skip continues to the
next alternative, while an infrastructure abort stops because it publishes no
alternative result or resource successor. Its terminal route states are
`SettledSuccess`, `SettledFailure`, `Skipped`, and `Aborted`; non-route
operations use `Finished`. Recording the terminal event, storing the operation
value, and publishing the settlement-appropriate outputs is the coordinator's
local linearization point. It does not establish exactly-once external effects.

## Validation and observability

Graph validation checks node roles, port direction, one producer per output,
exact value/completion shape, resource version monotonicity, completion-backed
preserved sequence, and executable acyclicity. `olangc --target dot` renders
ordinary, resource, actor, completion/control, executable, and constraint nodes
with distinct styles and directed ports.

`ProjectAttemptTrace` version 2 binds events to the project name, bundle digest,
target, policy, logical graph digest, and a fresh execution-attempt identifier.
`--project-trace-out PATH` stores the unsigned JSON diagnostic when HGraph mode
is selected. It is not an OWRECEIPT or attestation.

The integration suite runs graph and serial execution in isolated working
directories and compares exit status, stdout, normalized stderr, final values,
persistent Python and SQL state, environment mutation/read behavior, and full
filesystem snapshots. A test-only rendezvous also proves real worker overlap for
safe renderer tasks.

## Deliberately deferred optimization

The current resource transition model serializes read/read access. The next safe
optimization is a read-lease model in which a writer consumes every outstanding
reader-completion token. Broader parallelism additionally requires verified
backend-specific resource models. The runtime does not claim complete static
effect inference, automatic path extraction from arbitrary hosted source, or
safe arbitrary cross-runtime parallelism.
