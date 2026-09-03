# Unified Ostadix Intent Front Door V1

`o run`, `o optimize`, `o plan`, `o explain`, and `o inspect` are routed by the
repository-owned Bash dispatcher to the compiled `o-cli` orchestrator. The
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

## Optimization boundary

The initial evidence-gated optimization UI is:

```text
o optimize TARGET --route ROUTE_SET [--receipt PATH] [--json]
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
V1 neither caches nor reuses that winner on a later invocation. Its comparison
covers complete captured results and declared artifacts, not hidden filesystem,
network, device, or other effects, so it makes no universal semantic-equivalence
claim.

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

Validated executions are recorded by default below
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
