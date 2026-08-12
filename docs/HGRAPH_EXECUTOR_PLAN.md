# State-complete HGraph executor

The Rust hosted runtime executes OIR through a directed HGraph. The graph
coordinator is the default; `O_EXECUTOR=serial` selects the reference OIR
executor used by the differential conformance suite.

```text
.O -> OIR -> validated ExecutionPlan -> draft HGraph -> type/fidelity solve
   -> EvidenceBundleV4 -> admission compiler -> AdmittedExecution
   -> Coordinator
```

## Evidence-bound ordinary OIR admission

The ordinary OIR coordinator no longer accepts an arbitrary HGraph.
`Coordinator::new` accepts only `AdmittedExecution`, whose fields and
constructor are private to the digest-checking admission path. The evaluator
also compiles this admission when `O_EXECUTOR=serial` selects the differential
oracle, so changing the executor does not bypass pre-execution checking.

`EvidenceBundleV4` is a pre-execution certificate bundle. For each executable
operation it records type, effect-footprint, dispatch, capability-policy,
placement, failure-policy, and resource-demand contracts, plus a separate soft
cost estimate and the provenance of every fact. Only enforced,
compiler-verified, or trusted-adapter evidence can establish a closed effect
footprint. A user declaration can add conservative constraints but cannot
erase unknown hosted effects. Historical observations and costs can inform a
future ordering policy; they do not change which schedules are legal.
Before issuing that bundle, the analyzer validates registered backend identity
and special-invocation name/mode/arity, so digest consistency cannot admit
forged execution metadata.

The bundle binds canonical lowered OIR, the validated plan, the solved analyzed
graph, analyzer identity, the canonical backend-catalog specifications
referenced by the plan, the current executable and consumed legacy Python shim
artifacts, execution environment, and
a descriptive ambient `HostWorld` snapshot. The catalog projection establishes
specification identity only; it does not establish runtime discovery, health,
authorization, capacity, or readiness. The bundle deliberately labels the
first digest `lowered-oir-sha256`: evaluator APIs can receive an existing
`OIrProgram`, so v4 does not claim an original source-byte digest. Admission
rejects mismatched bindings, attaches seven pre-materialized
`AdmissionEvidence` nodes to every executable edge, validates the resulting
graph, and freezes it. Immediately before running, both the coordinator and
serial oracle recompute and check the runtime binding; both recheck again before
opaque/deferred operations.

The backend artifact binding distinguishes hashed files, missing paths,
non-regular paths, and unreadable paths, and includes the current executable;
execution admission rejects an unhashed current executable. Runtime rechecks
are path/environment snapshots, not an immutable execution substrate: v4 does
not pin an opened adapter or frozen child environment and cannot prove the
bytes/environment observed at spawn. It also does not bind caller initial-scope
shape/values, opaque state/generation inside an already-live actor, the full
external interpreter/toolchain closure, or a placement lease.
In v4, actor identity is only a serialization identity; all persistent hosted work
remains unknown, coordinator-lane, and conservatively attached to `HostWorld`.

`olangc FILE --target ir --explain-schedule` exercises this solve, analysis,
and admission path without dispatch. It prints exact digest bindings,
per-operation provenance, blockers, retained source-sequence reasons, and
static legal waves. Its admission report identifies the runtime snapshot as
`inspection-only`; it is not interchangeable with the evaluator's execution
snapshot. The inspection surface is ordinary-OIR-only in v4.

`olangc FILE.O --target ir --why P3` projects the same evidence-bound admission
onto one plan operation and its immediate dependency neighborhood; repository
command `o why FILE.O P3` is a thin route to that compiler surface and performs
no scheduler parsing. `--why` is mutually exclusive with
`--explain-schedule`. The result is inspection-only and nonexecuting: static
waves/layers are not runtime batches, and the query observes no readiness,
timing, worker identity, or overlap. Its source origin is a descriptive sidecar
for the exact parsed input, outside OIR, plan, evidence, and admission digests;
it neither authorizes execution nor promises stable identity or incremental
invalidation across edits.

This admission is distinct from an observation or receipt. `RuntimeGraphV1`
and `ExecutionReceiptV1` describe completed execution and carry no scheduling
authority; a prior receipt cannot authorize a new run. Project HGraphs and the
buffered Request scheduler also remain separate execution islands in v4.

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
resource/actor state ----------/                         -> Completion(plan node)
admission evidence -----------/                          -> written state successors
```

Every executable hyperedge has one distinguished ordinary result and one
successful-completion output. Resource/control outputs are synthetic scheduling
values and carry no `OValue`. Every output has exactly one producer. Admission
evidence is producer-free, materialized before execution, and attached as a
literal readiness input rather than consulted as detached metadata.

Resource access has two different shapes:

```text
Read:  latest writer R@n -----------------> Execute(Read) -> Completion(reader)
Write: latest writer R@n + reader completions -> Execute(Write) -> R@n+1
```

A read does not emit a successor resource version. Its completion joins the
open-reader frontier. The next writer drains that complete frontier before it
can advance the resource once.

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

Unknown hosted operations report reads and writes of `HostWorld` and
evaluator-local state. Ordinary unknown access is lowered as exclusive. The
narrow exception is a direct, attribute-free ephemeral member of a group whose
effective source policy is explicitly `autonomous`: its open, unknown evidence
remains visible, but `explicit-autonomous-unordered` semantics permit those
implicit state ports to be omitted. This allows already-started members to race
external effects and provides no rollback guarantee. A precise
host access additionally holds a shared `HostWorld` read lease: this prevents
overlap with ambient unknown work without serializing two disjoint precise host
resources merely because both are host-visible. A persistent shim also consumes
and produces its actor-state token. The live process registry does not expose a
trustworthy generation, so actor resource identity does not invent a constant
generation field.

After scheduler-visible alias expansion, lowering maintains one frontier per
resource: the most recent writer state and every reader completion since that
writer. Reads consume the writer state and join the frontier without changing
the version. A write consumes that state plus all open-reader completions,
emits the next version, and clears the frontier. This admits read/read sharing,
preserves read/write and write/write exclusion, and prevents aliases from
bypassing the dependency.

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
relaxed only in these cases:

1. Both operations are direct members of the same explicit `batch`, `all`,
   `any`, or `race` group.
2. Both operations are compiler-verified, read-only O-level `Load` operations
   outside a structural left-to-right `O` region.
3. Both operations are verified, deterministic, infallible, resource-free
   inline renderers from the trusted `html`, `markdown`, `text`, and `latex`
   set; both complete structural subtrees contain only literal text and
   recursively trusted renderers; and neither operation is a child of a
   structural `O` sequencing region.

Outside explicit group semantics, any unknown fact retains the completion
dependency. Group topology does not normally override resource conflicts. Only
the explicitly autonomous ephemeral contract above omits its implicit
`HostWorld`/evaluator frontier, and that is a semantic opt-in rather than proof
of independent effects.

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
It can be constructed only from an `AdmittedExecution`. Its `LocalWorker` lane
admits compiler-verified O-scope `Load` operations; attribute-free trusted
`html`, `markdown`, `text`, and `latex` inline renderers whose bodies contain
only literal text, already-settled Store children, and recursively trusted
renderers; and the explicit non-strict hosted contract described above. On the
coordinator thread, preparation freezes the relevant scope or already-
materialized splice inputs into immutable owned `PreparedTask` envelopes.
Evidence schema v4 binds each
operation to exactly one adapter ID: `o-scope-load/v1`,
`trusted-inline-renderer/v1`, `autonomous-ephemeral-shim/v1`, or
`coordinator/v1`. Dispatch evidence also records `strict-equivalent` or
`explicit-autonomous-unordered` semantics. Runtime preparation validates
that exact adapter against the admitted OIR; it cannot choose a different
adapter as a second scheduling authority.

For each graph execution containing local-worker operations, the coordinator
creates one fixed-size pool and reuses its threads across changing readiness
frontiers. Without an explicit override, capacity is
`min(available_parallelism, admitted_max_local_worker_wave_width).max(1)`;
if the host query fails, `available_parallelism` falls back to one. An explicit
override replaces the derived value and is not clamped to either the reported
host count or the admitted width. A graph with no admitted local-worker
operations creates no pool, even though the shared count resolver has a minimum
result of one. The admitted quantity counts only local-worker operations in
each static Kahn wave. It is a sizing heuristic and not evidence-backed CPU or
memory admission or a bound on the completion-driven dynamic frontier. Each
physical completion is delivered independently.
The coordinator buffers the provisional outcome, settles every now-eligible
semantic prefix result, recomputes readiness, and may submit newly exposed
worker work while an unrelated prior task is still running. A reported static
wave is therefore not a pool batch, capacity promise, or observed completion
order. Coordinator-owned work remains single-owner and this bounded
implementation waits for the local pool to become idle before executing it.

A successful worker whose hard contract is both compiler-verified pure and
infallible may publish its value and HGraph outputs provisionally before its
deterministic trace frontier. This lets an infallible worker-only dependent
pipeline advance behind an unrelated slow operation. If an earlier semantic or
infrastructure failure wins, the coordinator removes every such output, clears
the frame value, and records the provisionally published operation as
discarded. `NodeFinished` remains the durable-settlement event, so a
provisionally unlocked dependent may start before its producer's
`NodeFinished`; the admission explanation makes that trace ordering explicit.
Fallible outcomes never use this early-publication path.

Same-binding loads share the latest writer frontier and demonstrably execute at
the same time in the local pool. Loads are pure but fallible. The coordinator
uses serial topological rank rather than preorder plan identity, and admits a
fallible task only while it extends the contiguous unfinished semantic prefix.
Physical completions may arrive in any order, but outcomes remain provisional
until the semantic frontier reaches them. Successful outcomes before the first
failure are materialized; the selected failure is reported; every later started
outcome is drained, recorded as discarded, and publishes no graph outputs.
Infallible effect-free workers may be dispatched outside the prefix. This
preserves deterministic strict fail-stop selection without requiring adjacent
pure reads to execute serially. Errors returned by admitted fallible adapters
remain semantic outcomes; an error from an admitted-infallible adapter is an
infrastructure contract violation. A broken pool mechanism is likewise an
infrastructure abort: the coordinator stops dispatch, drains every started task
to one terminal trace event, and does not disguise the failure as `NodeFailed`.
An unwind-capable build gives a caught worker panic the same treatment. The
release profile uses `panic = "abort"`, so
a release worker panic terminates the process before in-process recovery or
terminal trace completion; v4 does not claim otherwise.

After each accepted success, the coordinator materializes the value,
completion, and written successor-state outputs. On a selected failure, it emits
none of that operation's outputs and admits no later dependent operation. Root
values and scope writes commit in deterministic source order, but commit order
is not used to justify early external effects. Ordinary hosted reads and all
persistent language environments remain coordinator-owned. Before an admitted
autonomous hosted task is submitted, the coordinator revalidates runtime
artifacts, resolves live backend authority, and freezes the sandbox policy into
the task.

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
exact value/completion shape, access-mode-specific resource inputs and outputs,
resource version monotonicity, complete reader drains, completion-backed
preserved sequence, and executable acyclicity. Admission validation additionally
requires exactly one materialized token of each evidence-fact kind per
operation. `olangc --target dot` renders the solved draft graph's ordinary,
resource, actor, completion/control, executable, and constraint nodes with
distinct styles and directed ports. The separate
`--target ir --explain-schedule` surface compiles admission and reports its
evidence inputs and digest bindings without execution. It also emits an
advisory `ScheduleRealizability` marker under schema
`oexec.realizability/v1`. `admitted-static-max-wave-width` counts all operations
in the widest admitted static Kahn wave; `admitted-max-local-worker-wave-width`
counts only its `LocalWorker` subset and drives default pool sizing.
`worker-count-covers-static-wave` is `not-applicable` when the latter width is
zero, `yes` when `selected-workers` covers it, and `no` otherwise. The marker
also distinguishes `machine-default` from an unclamped `cli-override`; a failed
host parallelism query is displayed as the conservative fallback `1`. This
marker is intentionally outside the admission digest and states
`execution-realizable=unknown`, runtime availability unknown, no placement
lease, and no dispatch or overlap observed. Even a `yes` coverage value proves
no simultaneous dispatch, CPU/memory fit, runtime readiness, placement, or
dynamic-frontier bound.

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
filesystem snapshots. Test-only rendezvous probes prove real worker overlap for
safe renderer tasks and same-binding O-scope loads; isolated CLI integration
tests use monotonic intervals to prove overlap for explicitly autonomous
ephemeral Python members while retaining ordinary strict fail-stop behavior and
worker-side `O.eval` lexical scope. Pool tests prove fixed
capacity, thread reuse, singleton off-owner placement, independent completion
delivery, and recovery after a caught worker panic. Coordinator tests prove
that a fast worker can expose dependent worker work while an unrelated slow
task remains active. Fallible-worker tests preserve lowest-semantic-ordinal
failure selection and later-outcome discard while tasks overlap. Additional
regressions prove provisional-output revocation after an earlier failure and
that an infallible-adapter error immediately enters the infrastructure-abort
path without becoming `NodeFailed` or preempting an earlier semantic failure.
`scripts/benchmark_hgraph_hosted.sh` alternates release-mode serial and graph
runs across four fixed shapes: heterogeneous Python/Bash/Node work, a width-one
dependency chain, a `1 -> 4 -> 1` diamond, and a staged realistic pipeline. It
feeds each exact rendered fixture through the non-executing evidence/admission
path and consumes the versioned `oexec.schedule-prediction/v1` hosted-task
width/span projection before timing. It fails closed on an invalid prediction,
records the analyzer and execution-binary digests plus the prediction's
admission digest, emits per-pair timings and distributions, and checks exact
serial/graph/expected-output equivalence without imposing a speedup threshold
or treating one machine measurement as a portability claim.

## Deliberately deferred optimization

The reader-frontier topology now drives actual persistent-pool overlap for its
narrow compiler-verified O-scope `Load` class, per-completion wakeups remove the
previous complete-wave barrier, and explicit autonomous groups can opt bare
ephemeral hosted members into non-strict worker overlap. Enforced strict hosted
effect contracts, actor-owned persistent environments, renewable
CPU/memory/device capacity admission, critical-path or measured-cost ranking,
transactional prepare/commit, overlap between coordinator-owned operations and
outstanding worker tasks, and unified OIR/Request/project scheduling are future
work. Broader dispatch also requires verified or enforced backend-specific
effect and failure contracts.
The runtime does not claim complete static effect inference, automatic path
extraction from arbitrary hosted source, arbitrary hosted-read overlap, or safe
arbitrary cross-runtime parallelism.
