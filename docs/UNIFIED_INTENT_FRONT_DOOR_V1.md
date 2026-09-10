# Unified Ostadix Intent Front Door V1

`o run`, `o routes`, `o optimize`, `o plan`, `o explain`, `o inspect`,
`o object`, and `o operation` are routed by the repository-owned Bash dispatcher
to the compiled `o-cli` orchestrator. The
dispatcher remains necessary on case-insensitive macOS filesystems where `O`
and `o` cannot be separate installed filenames. Direct `O`, `olangc`,
`o-link`, node, registry, information, live, receipt, kernel, `o why`, and
unknown-argument evaluator behavior remain compatibility surfaces.

## Supported execution inputs

- An ordinary `.O` file executes through the in-process Parser/Evaluator API.
- A project directory is assembled losslessly and executes through its exact
  selected project route policy.
- A lifted project `.O` is detected by its embedded bundle before ordinary O
  parsing and executes through project routes.
- A standalone foreign file is rejected with guidance to bundle its containing
  codebase with `o-link --project`.

`o run FILE.O --parallel auto` forces the local HGraph worker pool. It performs
no peer discovery and no remote RPC. `o run PROJECT --parallel auto` selects
project mesh `prefer`; `--mesh=required` is the explicit mandatory-remote
spelling. Automatic and explicit mesh spellings conflict. No command in this
front door starts `o-node`; mesh execution can use only already-running,
authenticated peers and its configured retry/fallback policy.

## Semantic-record inspection boundary

`o operation` is a separate, non-executing front-door namespace for the
experimental operation-realization V1 records:

```text
o operation inspect <contract|interface|descriptor|set> FILE [--json]
o operation verify \
    --contract FILE \
    --interface FILE \
    --descriptor FILE [--descriptor FILE ...] \
    --set FILE \
    [--json]
```

`inspect` validates one explicitly typed `OperationContractV1`,
`OperationInterfaceV1`, `RealizationDescriptorV1`, or `RealizationSetV1`.
References remain unresolved. `verify` requires the exact descriptors declared
by the supplied set and checks interface-to-contract, descriptor-to-pair and
port-coverage, set-to-interface/contract/descriptor, and unique-stable-name
consistency. Human success says `Referential consistency: PASS`; JSON success
reports `referentially_consistent` in an
`ostadix.operation-verification/v1` envelope.

The explicit kind or option chooses the record decoder. A first
non-whitespace `{` selects bounded JSON; otherwise strict canonical CBOR is
required. The CLI never infers kind from a filename or supplies an implicit
bundle, catalog, registry, or path convention. Record failure exits 1; command
usage failure exits 2.

Every operation-record input file is capped at 4 MiB. `o operation verify`
checked-sums the raw bytes of the contract, interface, every supplied
descriptor, and set files, and rejects an aggregate input closure above 64 MiB.
The realization set and supplied descriptor list are independently capped at
65,536 members and must have exactly matching counts. The 64 MiB CLI resource
limit is not part of any record schema or identity.

This surface validates declarations and their exact references only. It does
not resolve artifacts, derive an interface from source, plan or select a
realization, prove behavioral equivalence, authenticate evidence, establish
target eligibility, place or transfer work, schedule or execute it, observe or
recover runtime state, or grant evidence, admission, capability, lease, or
World authority. See `docs/OPERATION_REALIZATION_V1.md`.

## Optimization boundary

Route discovery is a separate, read-only operation:

```text
o routes TARGET [--json] [--route-decl DECL]...
```

For a project directory or lifted project bundle it reports safe, ordered
metadata only: route IDs, kinds, result codecs, and explicitly declared route
sets with their first alternative identified as the reference. Each route set
is marked structurally ready or rejected for
`benchmark_validate_and_select`, and separately reports whether its transitive
routes meet the declared-pure boundary needed for later winner reuse. Discovery
never executes a route, creates or
opens run state, infers a set from routes that merely share `provides`, or
prints commands, environment values, guards, labels, or source bytes. JSON is
one versioned `ostadix.route-catalog/v1` object with separate
`optimize_ready`/`optimize_rejection` and
`reuse_ready`/`reuse_rejection` fields for each route set.

The initial evidence-gated optimization UI is:

```text
o optimize TARGET --route ROUTE_SET [--receipt PATH] [--progress auto|always|never] [--json]
o run TARGET --selection-run RUN_ID [--json]
```

The route set is explicit and required in v1. `TARGET` must expose that named
project route set; the command does not guess a default comparison set.
Repeated `--route-decl DECL` values can supply explicit route declarations,
and `--receipt-out` is an accepted spelling of `--receipt`.

`o optimize` implies project execution, fixes the route policy to
`benchmark_validate_and_select`, and requires durable run recording. It does
not expose mesh, parallelism, executor, alternate-policy, or no-record controls.
The first declared alternative is the reference, and every candidate runs in
an isolated workspace. A candidate is eligible only when it settles
successfully and its complete captured result and declared artifact manifest
match the reference under the route set's declared-output contract. The
fastest eligible complete branch is selected. Missing or incomplete evidence
fails closed according to the underlying policy rather than silently admitting
a faster route.

The default presentation is a human candidate summary in declaration order.
It identifies the reference and selected routes; shows eligibility or a
sanitized rejection reason and complete-branch duration for every candidate;
reports the measured reference/selected speedup ratio when defined; and prints the
receipt digest plus the durable run ID consumable by `o inspect`. It explicitly
states that all candidates ran and, when applicable, that none beat the
reference during this validation run.

`--json` emits exactly one `ostadix.optimize-summary/v1` object with `schema`,
`run`, `receipt`, `receipt_sha256`, and `receipt_export_path`. `run` is the
existing `ostadix.run-summary/v1` object. `receipt` is the typed
`ostadix.project-validated-selection/v1` value when available; structured
preflight and recording failures retain the same envelope and use `null` for
unavailable receipt fields.

The receipt is always embedded in the required durable run record when
selection succeeds. `--receipt PATH` additionally exports its canonical bytes;
that destination must be outside the project input and cannot replace a lifted
project file. Omitting the export does not disable recording.

This is an evidence-gathering invocation, not same-invocation acceleration:
the reference and every candidate have already run before a winner is known.
For human output, `--progress auto` reports safe candidate settlements on the
original terminal stderr, `always` forces that view for non-terminal stderr,
and `never` suppresses it. Progress is excluded from candidate captures and
JSON stdout; `--json --progress always` is rejected before execution.

`o run TARGET --selection-run RUN_ID` is a distinct, later-invocation reuse
path. It accepts only an exact successful terminal run loaded and revalidated
through the private content-addressed run store: `last-run` and exported
receipt files are not authority. The current bundle, benchmark HGraph and
deployment identities, route declarations, target, ordered alternatives,
reference, winner, and expected declared-output digest must match. Ostadix
then derives an explicit winner plan from that same in-memory bundle, pins the
preflighted local compatibility executor, and revalidates every mutable
prepared-project coordinate immediately before dispatch.

Only the selected top-level branch is dispatched (its declared prerequisites
may run), and the CLI front door requires and finalizes a fresh durable record.
The selected branch's declared output is recomputed after execution. A route
failure, malformed observation, or digest mismatch is a typed terminal
failure; no candidate is retried and no fallback executes.

Reuse additionally requires every route-set alternative and every transitive
prerequisite to carry the bundle author's explicit `pure = true` declaration.
That is an auditable assertion, not independently enforced sandbox evidence.
The output check occurs after execution and cannot undo undeclared filesystem,
network, device, or other effects, so V1 makes no universal
semantic-equivalence or transactional-substitution claim.

Systems can embed this without parsing CLI text by calling
`RunStoreReaderV1::read_terminal_verified`,
`prepare_selection_reuse_intent`, and `execute_prepared_intent`. The resulting
`PreparedSelectionReuseV1` is opaque and non-serializable; durable observations
bind the source run object, receipt, reuse contract, selected route, and
postcondition. These library calls do not automatically begin or finalize a
run-store transaction: execution returns the typed reuse observation, and an
embedder that requires durable audit evidence must persist it itself.

## Planning boundary

Planning is static unless `--live` is supplied. Static planning reads the input
and builds the same OIR/HGraph or Project HGraph/deployment view as
`olangc --target ir`; it does not open run history or discover peers. Ordinary
`.O --live` adds local runtime/worker readiness only. Project `--live` may read
LAN advertisements as endpoint hints for identities already present in the
pinned peer registry, then issue authenticated profile and capacity queries.
It cannot enroll a peer, upload CAS data, probe a route, create a fence, submit
an actor, read a result, execute a command, or start a node.

## Private observations

Validated executions through the CLI front door are recorded by default below
`${XDG_STATE_HOME:-$HOME/.local/state}/ostadix/runs-v1`. Preflight failures do
not allocate a run ID and do not change `last-run`. `--no-record` never opens
the store. `--require-record` aborts before execution when recording cannot
begin and exits nonzero after execution when finalization fails; computation is
never replayed.

The store uses owner-private directories/files, no-follow checks, canonical
CBOR, immutable content-addressed record/trace objects, atomic rename and
synchronization, short global transactions, and per-run leases. Later writers
reconcile released orphan leases as interrupted; inspection is strictly
read-only. Retention is bounded to 128 attempts and 256 MiB of referenced
objects, with active leases protected.

Records retain the bytes already retained by a runtime, complete-stream
length/digest/truncation metadata, decoded values, route results, and artifact
path/hash/size/completeness metadata. They never retain source trees, project
bundles, ambient environment values, credentials, or artifact payload bytes.
Every record is explicitly `integrity=unsigned_observation`: it is not
admission, a signature, an OWRECEIPT, or World authority.

`o explain [last-run|RUN_ID]` renders a validated causal narrative.
`o inspect [last-run|RUN_ID]` emits the validated record as JSON, and `--trace`
also resolves and validates the content-addressed trace attachment. Neither
command executes or discovers anything.
