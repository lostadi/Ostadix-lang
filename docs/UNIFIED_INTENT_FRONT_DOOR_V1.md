# Unified Ostadix Intent Front Door V1

`o run`, `o plan`, `o explain`, and `o inspect` are routed by the
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
