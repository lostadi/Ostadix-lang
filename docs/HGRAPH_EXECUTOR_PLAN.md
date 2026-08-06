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

ProjectHGraph -> LogicalHGraphV1 -> DeploymentPlanV1(hosted-unbound)
                                  -> ProjectAttemptTrace v5 header binding

LogicalHGraphV1 + PlacementSnapshotV1 + DeploymentPlanV1(snapshot-derived)
  -> HostedWorldLaunchV1 + HostedWorldCurrentV1
     (coordinator observer + distinct coordinator attempt + operation attempts)
  -> ProjectCoordinator::new_world_bound (all fences before workspace/child)
  -> ProjectAttemptTrace -> causal replay -> terminal RuntimeGraphV1
  -> caller-signed OWRECEIPT v1 (ReceiptCommitFenceV1::Uncommitted)
  -> Mode 32 native canonical/semantic comparison
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

## Deployment intention boundary

World PR8-2 adds `DeploymentPlanV1` as a canonical intention record bound to
the exact `LogicalHGraphV1` digest. The opt-in hosted executor derives only the
hosted-unbound form. For coordinator-supported `Explicit`, `Default`,
`Fallback`, and `AnySuccess` policies, `BuildRoute`, `SelectRoute`, and
`CompareRouteResults` are `HostedCoordinator`; `MaterializeProject` and
`RunRoute` are `AmbientHost`. The record carries no World, task, node, domain,
process, provider, or placement identity. Unsupported hosted policies remain
`Unresolved`; they do not acquire a placement by using the compatibility
runtime.

The active requirements derived from the logical graph are the exact project
bundle, bundle-scoped role/path declarations, runtime classes,
executable/evaluator facts, platform and ambient-environment guards, explicit
authority absence, and residual `HostWorld` admission. Bundle-provided
environment-overlay key names are recorded separately from ambient environment
requirements. Architecture, package, and failure-domain fields are canonical
schema vocabulary, but the current project logical profile leaves them
unconstrained or empty.

The separate `from_snapshot_single_provider` constructor requires a
caller-supplied exact World-epoch `PlacementSnapshotV1` and one caller-supplied
exact `TaskIdentity` for each logical operation. It deterministically emits a
`ProposedProvider` or `Unresolved` result from those descriptive facts. This is
not current or authenticated inventory, Governor admission, authority,
dispatch, reservation, provider health, or execution. `require_current_world`
checks only World identity/epoch. The ordinary opt-in executor continues to use
the hosted-unbound plan, but the separate hosted-reference World entry point
does consume the exact snapshot-derived plan. `HostedWorldLaunchV1` binds the
logical, deployment, snapshot, World, descriptive Governor position, selected
provider, receipt identity, caller-supplied coordinator observer, dedicated
coordinator attempt, and one exact task attempt per logical operation. The
coordinator attempt must use a task distinct from every operation attempt.
`HostedWorldCurrentV1` fences the World/Governor generations, observer
node/domain/optional-process generations, coordinator attempt, provider
node/domain/optional-process/service generations and implementation digest, and
every operation attempt before `ProjectCoordinator` derives its schedule,
materializes a workspace, or starts a child. This is a caller-supplied
current-view comparison, not authenticated membership, proof that the host owns
the observer identity, or Governor admission.

After coordinator completion or an observable coordinator failure,
`RuntimeGraphV1` canonically binds the logical/deployment/launch/snapshot
schemas and digests, exact World/observer/coordinator-attempt/provider and
per-operation task-attempt context, normalized trace event ordinals and
outcomes, and each operation's residual `HostWorld` truth. Construction first
runs plan-aware causal replay against the trusted HGraph and exact deployment.
Never-started operations retain empty observations. The neutral
`RouteSettlement` terminal represents successful, nonzero, or guard-skipped
route settlement; it is not synonymous with successful execution. Its residual
`HostWorld` bit is aggregated across all actually observed started or terminal
operations. The graph is terminal hosted-reference evidence, not mutable live
state, authority, a recovery plan, or a commit decision.

`execute_world_project_with_receipt` then uses a caller-supplied Ed25519 signer
to emit canonical `OWRECEIPT` v1 with
`ReceiptCommitFenceV1::Uncommitted`. The receipt placement is the coordinator
observer and its attempt is the dedicated coordinator attempt, not the proposed
provider or a per-operation attempt. The receipt subject binds the bundle and
logical graph while leaving package absent; the provider implementation is not
overloaded into that field. Route success maps to receipt success, while
nonzero and guard-skipped settlements map to receipt failure. Signing protects
integrity but does not turn the descriptive Governor/provider context into
admission, authority, or a governed commit.

Mode 32 accepts that emitted receipt as canonical lowercase hex, performs full
native canonical decode, exact re-encoding, validated signing-preimage
construction, requires the uncommitted fence, and compares the
domain-separated SHA-256 of the complete unsigned canonical body with the
hosted value. It also reuses successful validation scratch with a malformed
envelope and requires stale terminal/commit tags to be cleared. The first
command is the required no-argument end-to-end wrapper; the second is the
direct two-argument vector interface:

```bash
./ocore/kernel/smoke-world-project-runtime-qemu.sh
./ocore/kernel/smoke-world-project-receipt-qemu.sh RECEIPT_HEX_FILE EXPECTED_SEMANTIC_SHA256
```

This is no Governor admission/commit, capability grant, lease, reservation,
remote dispatch, recovery, or exactly-once protocol. Mode 32 does not execute a
project or verify Ed25519 natively. QEMU TCG is not physical-hardware evidence,
and this slice passes neither G1 nor Workstream A acceptance. G1 remains defined
and unpassed.

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
`Started` attempt events. When the terminal alternative settles unsuccessfully,
continuation is admitted only when every route child was guard-skipped or every
executed route in that branch, including successful prerequisites, carries the bundle-bound
`failure_continuation = "declared_idempotent"` contract. The field defaults to
`unproven`, which stops before the next branch. An infrastructure abort also
stops because it publishes no alternative result or resource successor. A
nonzero prerequisite also hard-stops: this slice does not synthesize a
branch-terminal result or continuation decision from a failed prerequisite,
regardless of its declared contract. Its terminal route states are
`SettledSuccess`, `SettledFailure`, `Skipped`, and `Aborted`; non-route
operations use `Finished`. Recording the terminal event, storing the operation
value, and publishing the settlement-appropriate outputs is the coordinator's
local linearization point. The idempotency contract is an author declaration,
not a verified sandbox, effect log, fence, compensation protocol, or proof of
exactly-once external effects. This admission rule exists only in the opt-in
hosted HGraph coordinator; it does not change the compatibility runtime.
Project-bundle format v2 carries the continuation contract. Legacy v1 bundles
migrate only when all routes omit that field and then default to `unproven`;
v1 documents carrying the v2 field and mislabeled serializer inputs are
rejected.

## Validation and observability

Graph validation checks node roles, port direction, one producer per output,
exact value/completion shape, resource version monotonicity, completion-backed
preserved sequence, and executable acyclicity. `olangc --target dot` renders
ordinary, resource, actor, completion/control, executable, and constraint nodes
with distinct styles and directed ports.

`ProjectAttemptTrace` version 5 binds events to the project name, bundle digest,
target, policy, canonical `LogicalHGraphV1` schema/digest, exact canonical
deployment schema/digest, and an execution-attempt identifier. The ordinary
path binds the hosted-unbound plan and uses a fresh diagnostic identifier. The
World-bound path binds the exact snapshot-derived plan and uses the launch's
dedicated coordinator attempt identity, which is distinct from every logical
operation attempt and remains descriptive and non-authorizing.
For an unsuccessful ordered branch it also records the proposed next route,
the assessed route prefix, the `no_execution`, `declared_idempotent`, or
`unproven_effects` evidence class, and the allow/deny result.
`--project-trace-out PATH` stores the unsigned JSON diagnostic when HGraph mode
is selected, including a denied decision before the command reports no
successful route. Structural replay alone checks only event-local lifecycle
invariants. Plan-aware replay against a trusted `ProjectHGraph` additionally
checks all header bindings, reconstructs the ordinary hosted-unbound deployment
artifact or compares the explicitly supplied snapshot-derived artifact, rejects
substitution, checks exact operation identities,
requires the decision on the correct terminal alternative, requires complete
causally ordered lifecycle coverage for every transitive route prerequisite,
recomputes its evidence from `RoutePlanFacts`, checks the exact next alternative,
and rejects later-branch events after denial. Every complete
coordinator-produced trace passes that semantic replay before it is returned.
The trace itself remains unsigned and is not an OWRECEIPT or attestation; only
the separate terminal RuntimeGraph/receipt adapter emits the signed,
uncommitted receipt described above.

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
