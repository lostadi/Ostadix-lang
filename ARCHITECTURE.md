# Ostadix-lang Architecture

Ostadix-lang is a universal polyglot expression framework where every expression
syntactically declares which language it is written in.

```
html^( <p>Result: python^( 2 + 2 )_python</p> )_html
```

## Repository Layout

```
Ostadix-lang/
├── crates/ostadix-api/ # Independently packageable runtime engine
│   ├── src/          #   Parser, IR, values, evaluator, HGraph, admission,
│   │                 #   scheduler, hosted/project/World, information, O-core
│   ├── backends/     #   Authoritative embedded backend shim assets
│   └── test-assets/  #   Package-local benchmark and native ABI fixtures
├── src/              # `o-lang` compatibility and CLI shell
│   ├── lib.rs        #   Explicit historical-module reexports from the engine
│   ├── main.rs       #   O interpreter entry point
│   └── bin/          #   Additional binaries (olangc, ocorec, o-link, …)
├── ocore/            # Native systems runtime and bootable x86_64 kernel proof
├── okernel-multikernel/ # Foreign-kernel personality proposal & boot-and-test entrypoint
├── backends/         # Byte-identical CLI/source compatibility shim mirror
├── examples/         # .O example programs
├── c_cpp/            # Complete C17 port (standalone)
├── o_lang/           # Legacy Python prototype (reference only)
├── tests/            # Rust integration tests plus Python-era legacy tests
├── fuzz/             # Parser fuzz targets and seed corpus
├── setup/            # Cross-platform bootstrap scripts
├── scripts/          # Repository management scripts
├── docs/             # Design documents and brainstorms
├── boot-and-test.sh  # One phased entrypoint over every build/boot/test layer
├── SPEC.md           # Language specification
└── README.md         # Project overview
```

## Frozen dependency direction

Runtime dependency flow is one-way: the root `o-lang` package depends on
`ostadix-api`; the engine never depends on the shell. Historical
`o_lang::<module>` paths are explicit reexports of the engine's same nominal
types, so the extraction creates no second evaluator, registry, value model, or
admission compiler. Inside that engine, the cycle-breaking boundaries remain
executable repository contracts:

- parser production code depends only on `syntax_dialect.rs`, a narrow view
  answering registration, canonical spelling, and quoted-body ownership;
- `crates/ostadix-api/src/ir.rs` owns OIR and plans but does not import HGraph or placement;
  those higher projections consume IR, and the historical inherent HGraph
  convenience methods are implemented in `crates/ostadix-api/src/hgraph/from_oir.rs`;
- `dispatch_model.rs` is a pure classification layer shared by evidence and
  executor, so evidence no longer imports worker implementation;
- governed identity is rooted below both Effects and World (the historical
  `world::identity` path is a compatibility re-export);
- the physical `crates/ostadix-api/src/placement/protocol/` tree is loaded exactly once as the
  crate-private `placement_protocol` module. It remains below IR projections,
  Registry integration, execution, and World. Public
  `placement::SemanticDigestV1` and
  `placement::protocol::SemanticDigestV1` are compatibility aliases of that
  same Rust type, not separately compiled protocol copies;
- `crates/ostadix-api/src/backend_catalog.rs` is compiled exactly once as the crate-private
  canonical catalog implementation. IR's historical named exports and
  `registry::bundle` are public compatibility projections of those same Rust
  types; internal library code imports the canonical module directly. Catalog
  integration depends on the registry-independent `placement_protocol` module,
  while registry storage and compatibility adapters remain above it. Generated
  AOT runtimes preserve the same single-identity geometry;
- `crates/ostadix-api/src/execution_contract.rs`, `crates/ostadix-api/src/eval_core.rs`, and
  `crates/ostadix-api/src/backend_state.rs` are canonical lower seams for execution policy,
  evaluator-independent graph execution, and backend lifecycle state. Their
  historical public paths preserve type identity without recompiling the
  implementation or reopening the higher realization roots.

[`ci/architecture-roots.toml`](ci/architecture-roots.toml) is the executable
root-dependency contract. It declares every current production root, allowed
cross-root edge and layer, the non-conventional `resource_identity` and
`placement_protocol` physical mappings, and compatibility facades. The
token-aware `python3 scripts/check_architecture_boundaries.py` enumerates all
production engine module files under `crates/ostadix-api/src`, verifies the
crate-root declarations and their physical paths against the library target in
`crates/ostadix-api/Cargo.toml`, and permits only its exact public
`api::{...}` value projection beyond external module declarations. It scans the
compiled `backend_catalog.inc.rs` token fragment under its owning root, rejects
symlink geometry, and rejects every undeclared direct or production-active
conditional `#[path]` and every direct or aliased `include!` source escape.
Physical overrides are explicitly
enumerated even when their filename does not end in `.rs`; each maps exactly
one crate root, cannot overlap an exclusion, and cannot retain a second
conventional `mod child;` owner. Directory overrides require a regular
`mod.rs`; file overrides cannot declare external children because their
descendant filesystem convention would re-enter the displaced root. External
child declarations nested inside inline modules or block items are rejected
because that physical module stack is not part of the manifest model. The
exclusion set is closed to the engine library entrypoint and declared compiled
fragments; it contains no shell binary or directory exclusion. The checker also
enforces the package direction: `ostadix-api` cannot depend on `o-lang`, while
the root compatibility shell must use the exact same package version and exact
path dependency, re-export every public engine module, and contain no duplicate
runtime implementation outside its CLI entrypoints.
Facades bind an unconditionally public owner and exact public alias or glob
projection. The checker then rejects undeclared roots and edges, retains
the narrow semantic rules above, and runs Tarjan's algorithm over the observed
root graph. The frozen baseline contains 152 production engine module files,
41 roots, 179 cross-root edges, and zero multi-root strongly connected
components. The separately scanned include fragment has no cross-root edge.

That is a root-level acyclicity claim only. Dependencies and bounded strongly
connected components may remain among files or modules inside one declared
root; the guard deliberately collapses them to that root. Moving the graph into
an independent crate therefore does not turn this result into a file-level
acyclicity claim.

The proof surface is the enumerated, token-level production source plus each
declared compiled fragment; it is not a `rustc` expansion trace. External
procedural or declarative macros that synthesize paths without a lexical
`crate`/`super` token remain outside this claim. Locally visible macro
invocations carrying either root token fail closed instead of being interpreted
as ordinary paths. Macro matchers and `$name` metavariable references are
treated as non-emitted syntax, but a literal `mod` keyword in a transcriber or
direct invocation fails closed—even when that macro is unused or conditionally
inactive—because macro-generated physical module ownership is unsupported.

The experimental authority-free information substrate is described in
[`docs/INFORMATION_KERNEL_V1.md`](docs/INFORMATION_KERNEL_V1.md). It references
existing native records without changing their bytes or execution authority.
Its raw V2 provenance witnesses are additive sidecars over V1 atom identities;
V1 atom, snapshot, revision, delta, pack, and digest bytes remain unchanged.
Contextual provenance analysis is isolated in the layer-13
`information_provenance` root, whose complete direct dependency image is
`evidence`, `information`, `ir`, and `world`. It returns opaque admitted handles
and cannot depend on `information_bridge`. Conversely, the authority-free
bridge retains its prior dependency budget and cannot import the provenance
admission root. The distinction between raw predicate validation and contextual
image admission is specified in
[`docs/IMAGE_ADMISSION.md`](docs/IMAGE_ADMISSION.md).

## Evaluation Pipeline

Ostadix-lang processes hosted code through a 7-stage pipeline:

1. **Parse** — Tokenize source into typed expression trees. Each expression
   carries a language tag (e.g., `python`, `html`, `nix`).

2. **Lower** — Convert the syntax-only `ONode` forest to executable OIR.
   Every `Exec` instruction freezes the backend's canonical identity, purity,
   splice renderer, and dispatch mode.

3. **Plan** — Build and validate `ExecutionPlan`. Structural edges connect
   children to parents, sequence edges preserve source order, and data edges
   connect loads to their visible stores.

4. **Project and solve** — Lower executable operations into a directed HGraph.
   Ordinary results, resource versions, actor state, and successful completion
   are nodes, then solve the graph's type, representation, and fidelity
   constraints.

5. **Analyze and admit** — Produce pre-execution type, effect, dispatch,
   capability, placement, failure, and resource-demand evidence. Bind it to
   the lowered OIR, plan, solved graph, backend artifacts, environment, and
   descriptive ambient-World snapshot; compile it into an immutable
   `AdmittedExecution`.

6. **Schedule, render, authorize, and dispatch** - The coordinator accepts only
   that `AdmittedExecution`, derives the dynamic ready frontier, converts child
   values with the renderer embedded in OIR, resolves the block's live backend
   capability, and executes the selected operation. Request values created by
   OIR carry compositional fingerprints into the existing eager/autonomous
   request scheduler; that scheduler remains a separate authority in V5.

7. **Settle and observe** — Materialize successful value, completion, and state
   outputs, select deterministic failures, and emit traces or receipts only
   after execution. `O_EXECUTOR=serial` retains the topological OIR interpreter
   as the differential reference semantics after the same admission has been
   compiled. Unifying Request and project execution under the admitted OIR
   coordinator is future work.

## Intermediate Representation (OIR)

`crates/ostadix-api/src/ir.rs` is the canonical hosted execution surface. It is the seam between
syntax (`ONode`), executable instructions (`OIr`), dependency planning
(`ExecutionPlan`), runtime values (`OValue`), and typed backend interfaces
(`BackendSpec` / `BackendInterface`):

- **`OIr` / `OIrProgram`** is the executable form of a parsed program.
  Lowering maps `RawText` to `Text`, `VarRef` to `Load`, `LetBinding` to
  `Store`, `Call` to `Invoke`, and `TypedExpr` to `Exec`. `Exec` also owns a
  `BackendInterface`, so runtime dispatch cannot drift from OIR analysis.
  `Invoke` owns an `InvokeMode`, so eager, lazy, autonomous, and group policy
  is decided during lowering rather than rediscovered by the evaluator.
- **`ExecutionPlan`** is the validated dependency graph built from OIR.
  Structural edges encode child to parent dependencies, sequence edges preserve
  left-to-right order, and data edges connect `load $x` to the latest visible
  `store $x`. It rejects invalid identities, out-of-bounds edges, duplicated
  roots, and cycles, then provides the stable topological root schedule and
  direct-child schedules used by the evaluator.
- **`BackendSpec` / `BackendRegistry`** provides centralized backend metadata:
  purity (whether `{lazy}` may cache results), the splice-rendering
  strategy used by `render_child`, typed dispatch mode (`inline_ast`,
  `inline_value`, `shim`), and shim path resolution
  (`<dir>/<lang>_shim.py`, `<dir>/<lang>_shim`, `<dir>/<lang>.py`,
  `<dir>/<lang>`, in that order).

`Evaluator::eval_document` and `eval_document_with_scope` lower immediately to
OIR and call the same OIR engine used by `eval_ir_program`. No production path
interprets `ONode`. `O.eval` callbacks re-enter through the parser, lower to a
new OIR program, validate its plan, and execute it through the same engine. The
callback root scope is a clone of the O bindings visible at the backend call
site. Reads therefore have lexical visibility, while callback `let` bindings
cannot mutate the caller. The evaluator retains the most recent validated plan
through `last_execution_plan()` for inspection and tests.

`scope()` materializes those bindings as a first-class OScope. The Python shim
can send OScope in an `eval_request`, so `O.eval(expr, scope_snapshot)` replaces
implicit capture with an explicit lexical root. OScope is distinct from OMap:
it carries namespace intent and is conservatively non-cacheable,
non-replayable, and non-persistable because its bindings may delegate live
capabilities or references.

OIR remains intentionally distinct from SSA. Recursive OIR regions preserve
lexical scope and policy-changing special forms such as `lazy`, `autonomous`,
and coordination groups. Every `Store`, `Invoke`, and `Exec` maps its direct
OIR children to plan identities before execution. The plan expresses legal
dependency order, while runtime Request values carry fingerprints into the
eager executor or autonomous scheduler.

O-core does not lower into this representation. Native `.oc` files use the
separate `AST -> typed HIR -> SSA MIR -> object` pipeline under `crates/ostadix-api/src/ocore/`.
This separation prevents machine-level mutation, layout, and control-flow
semantics from being conflated with OIR's backend dependency graph.

## Directed HGraph execution

`crates/ostadix-api/src/hgraph/from_oir.rs` derives one semantic effect summary before constructing
each executable edge. Every executable edge has one distinguished OValue output,
one successful-completion output, and zero or more successor resource-state
outputs. Its inputs include ordinary child/data values, materialized admission
evidence after admission, and the state/control inputs required by its access
mode. A read consumes the latest writer state without producing a successor
version and adds its own completion to that resource's open-reader frontier. A
write consumes the latest writer state and every open-reader completion,
produces the next resource version, and clears the reader frontier. Persistent
shim operations also consume and produce
`ActorState(canonical-language[environment])`.

Unknown hosted effects read and write `HostWorld`, which is a conservative
umbrella for host-observable state. The graph does not infer exact filesystem or
network footprints from arbitrary hosted source, so ordinary hosted blocks stay
strictly ordered. The narrow execution-topology exception is a direct,
attribute-free ephemeral shim member of a group under the effective
`autonomous(...)` policy. Its evidence remains open and unknown, while the
source opt-in permits omission of implicit host/evaluator topology edges and
selects `explicit-autonomous-unordered` dispatch. Explicit O dataflow remains
ordered; already-started filesystem, network, process, and evaluator effects
may race and are not rolled back. Persistent indexed environments retain host
and actor-state serialization.

### Evidence-bound admission

`crates/ostadix-api/src/evidence/` separates pre-execution certificates from post-execution
observations. `EvidenceBundleV5` records per-operation type, effect, dispatch,
capability, placement, failure, resource-demand, and cost contracts together
with provenance. Hard contracts determine whether execution is legal. Cost
estimates are soft evidence: they may eventually rank an already-legal
frontier, but they do not remove dependencies or close an unknown effect
footprint.

The bundle is digest-bound to canonical lowered OIR, the validated plan, the
solved analyzed graph, analyzer identity, the canonical backend-catalog
specifications referenced by the plan, consumed legacy Python shim files, a
V5 direct-executable manifest, the execution environment, and a descriptive
ambient `HostWorld` snapshot. For an execution snapshot, the manifest selects
each plan-used shim backend's direct launchers once, preserves their absolute
invocation paths, opens and hashes each canonical target once, records Unix
identity, and retains the handles through dispatch. On non-Unix platforms the
portable fallback re-hashes the canonical target immediately before launch
rather than removing backend capacity. It includes the current O proxy and the
macOS sandbox wrapper when consumed. The same backend-scoped manifest reaches
the proxy, so backend commands use admitted absolute paths instead of resolving
another `PATH` candidate. Inspection emits unprobed requirement rows and keeps
runtime readiness unknown. The catalog projection establishes specification
identity only; it does not establish health, authorization, capacity, or
readiness.
The analyzer first rejects lowered backend interfaces that do not match the
registered language policy and noncanonical special-invocation metadata, so a
digest cannot turn a consistently forged interface into valid evidence. The
current binding deliberately does not claim to hash original source bytes,
because public evaluator entry points may receive an already-lowered
`OIrProgram`. It also does not digest-bind the caller-owned initial scope shape
or values; those are installed after admission and frozen only when a narrow
O-scope `Load` task is prepared. Admission rejects stale or mismatched evidence, adds seven
materialized `AdmissionEvidence` inputs to each executable edge, validates the
result, and freezes it as `AdmittedExecution`. `Coordinator::new` accepts only
that type and rechecks its runtime binding before execution can emit a started
event.

This authority boundary is currently the ordinary OIR execution path only.
The buffered Request scheduler and `ProjectCoordinator` remain separate, and
enforced strict hosted effect contracts, renewable CPU/memory/device admission,
and actor-owned persistent environments remain future work. Evidence schema V5 binds
each dispatch contract to one stable preparation adapter ID. The runtime may
validate that exact adapter against the admitted OIR, but cannot reclassify the
operation through a second scheduling authority. The current `LocalWorker`
lane remains deliberately narrow: `o-scope-load/v1` prepares
compiler-verified O-scope `Load` operations, while
`trusted-inline-renderer/v1` prepares the trusted attribute-free `html`,
`markdown`, `text`, and `latex` inline renderers with source-closed bodies.
`autonomous-ephemeral-shim/v1` prepares only direct source-opted-in hosted group
members and carries a coordinator-resolved live sandbox policy.
`coordinator/v1` retains everything else, so a graph wave does not by itself
prove that every member ran on a worker thread. The backend
binding distinguishes hashed files, missing paths, non-regular paths, and
unreadable paths. The direct-executable lease checks retained-handle and
canonical-path identity immediately before spawn, detecting replacement and
in-place metadata drift; persistent actors are keyed by a backend launch
generation over their backend-scoped executable set, matching legacy shim
artifacts, and plan-independent child launch context. They are retired when
that generation changes, while unrelated backend additions do not restart
them. This closes ambient `PATH` alternative reselection, but it is not an
immutable execution substrate. On Linux, an ELF O backend proxy is launched
through its retained open object when procfs supports that route, with the
admitted invocation name preserved; an unavailable route falls back to the
freshly revalidated path. Scripts and foreign launchers remain path-executed to
preserve shebang, multicall, self-location, dynamic-loader, and toolchain
behavior, so their final same-principal verification-to-exec micro-window
remains. macOS has no general public handle-exec primitive. The manifest covers
direct launchers only, not shebang interpreters, compiler-driver subtools,
dynamic libraries, or descendants launched by hosted code. Consumed legacy
Python support files and adapter-owned tools are bound without constraining
subprocesses created by user code. Because `ExecutionPlan`
omits WebAssembly body shape, V5 conservatively binds the complete `wat2wasm`
plus runtime alternative rather than under-admitting a later WAT conversion. V5 also
does not attest a frozen child environment, the opaque state or generation of
an already-live actor, Request/project admission authority, or placement-lease
freshness. The separate Request authority captures and shares a Nix command
lease only when an uncached Nix Request is actually performed; lazy, cached,
and independent non-Nix members retain their prior capacity and failure
semantics.
`ActorResourceId` remains a
serialization identity in V5, and unknown actor work cannot use this gap to
remove `HostWorld` or actor dependencies.

Post-execution `RuntimeGraphV1` and `ExecutionReceiptV1` artifacts remain typed
observations with no scheduling authority. A historical receipt may inform a
future soft profile, but it cannot admit a later execution.

The governed-world identity foundation in `crates/ostadix-api/src/world/` now shares all 20
constitutional identity atoms with `ocore/world/identity.oc`. It separates a
World snapshot epoch from independently generated node, domain, process,
resource, object, and task-attempt identity. The bounded `OWIDENT` v1 corpus is
an identity-only byte oracle; serialized capability IDs remain descriptive
non-authority. The separate `OWPROTO` v1 foundation adds deterministic
architecture-independent records, strict 16 KiB and caller-selected bounds,
four fixed kinds, canonical nested identities, and offline schema negotiation.
Its 20-record, 1254-byte Rust/`.oc` corpus is a codec oracle, not a stream or
network transport, live handshake, authenticated session, authority channel,
OValue envelope, receipt, Governor, or consensus implementation. Mode 29 adds a
separate self-framed `OWVALUE` v1 layer rather than changing those four
`OWPROTO` v1 kinds. The bounded portable layer has a 4096-byte record maximum,
depth-16 and 128-node limits, an explicit allowlist, canonical ordered records
and scalar-key maps, a root-only inert versioned extension with a recursively
portable payload, and SHA-256 over the complete canonical record. Rust and
native `.oc` share a fixed 19-record, 928-byte oracle whose concatenated
SHA-256 is `264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc`,
and hosted projection rejects
authority-bearing, capsule, live-reference, request, and other effectful forms.
Mode 30 adds the separate self-framed `OWRECEIPT` v1 bounded execution-receipt
layer. It binds descriptive World identities and generations, content digests,
capability-right descriptions, terminal/commit fields, evidence-gate identity,
and an algorithm-tagged signature envelope into canonical records and
domain-separated signing preimages. Rust and native `.oc` converge on the fixed
two-record, 3,239-byte corpus (SHA-256
`1edd90bf881cd42d08e2031482baae4e7c9a95bd78cfa65f0cbe14147c0a2604`) and its
1,575-byte current and 1,546-byte stale signing preimages. A pinned public
conformance key drives real
hosted Ed25519 sign/verify and tamper tests. Native `.oc` converges on receipt and
signing-preimage bytes and rejects malformed signature envelopes, but does not
claim a general freestanding Ed25519 verifier or trusted signer policy. The
Mode 30 corpus remains an offline conformance fixture. A separate bounded
World-project hosted-reference path now emits a caller-signed canonical
`OWRECEIPT` after terminal project coordination, always with
`ReceiptCommitFenceV1::Uncommitted`.

That path enters `ProjectCoordinator::new_world_bound` only after re-deriving
the exact logical graph, snapshot-derived deployment, and placement snapshot and
fencing the caller-supplied current World/Governor; dedicated coordinator
observer node/domain/optional-process; dedicated coordinator attempt; selected
provider node/domain/optional-process/service and implementation; and every
operation task attempt. The coordinator attempt uses a task identity distinct
from every operation attempt and becomes the trace execution-attempt identity.
These checks occur before schedule derivation, workspace creation, or
child-process launch.

`RuntimeGraphV1` is constructed only after plan-aware causal replay of the trace
against the trusted `ProjectHGraph` and exact deployment. It binds the launch,
observer, coordinator attempt, proposed provider, per-operation attempts, and
normalized lifecycle/outcome observations. Its neutral
`RuntimeGraphTerminalV1::RouteSettlement` covers successful, nonzero, and
guard-skipped route results; it does not relabel every coordinator-returned
value as success. Terminal residual `HostWorld` is the aggregate over all
operations that were actually observed as started or terminal, not merely the
selected operation. Never-started operations remain present with empty
observations and do not contribute to that aggregate.

The OWRECEIPT context uses the caller-supplied coordinator observer as its
placement and the distinct coordinator attempt as its attempt. The selected
provider remains a descriptive proposal in the launch/RuntimeGraph; it is not
substituted as receipt placement. The receipt subject binds the project bundle
and logical graph with no package digest, so the provider implementation is not
overloaded into the package field. Route success maps to a receipt success;
nonzero and guard-skipped route settlements map to receipt failures. This is
provenance/freshness evidence from a non-authorizing hosted reference profile,
not Governor admission, provider reservation, capability or lease authority,
remote dispatch, recovery, or exactly-once execution.

Native Mode 32 accepts the emitted receipt as a bounded lowercase-hex record,
performs full canonical decode, exact re-encoding, validated signing-preimage
construction, requires the uncommitted fence, and compares the
domain-separated SHA-256 of the complete unsigned canonical body. Its
successful-record probe then reuses the validation scratch with a malformed
envelope and proves that prior terminal/commit tags were cleared. The required
no-argument gate generates the hosted vector and invokes the direct two-argument
vector interface:

```bash
./ocore/kernel/smoke-world-project-runtime-qemu.sh
./ocore/kernel/smoke-world-project-receipt-qemu.sh RECEIPT_HEX_FILE EXPECTED_SEMANTIC_SHA256
```

Mode 32 does not execute the project or verify Ed25519 natively. QEMU TCG is
not physical hardware, and Mode 32 passes neither G1 nor Workstream A
acceptance.

The hosted PR6 `ResourceKey` vocabulary now distinguishes World, Governor,
node, domain, process, generic resource, object, descriptive capability,
namespace, task-attempt, artifact-publication, device, and accelerator state.
Each class carries the existing validated World identity type rather than an
unvalidated string. Device and accelerator access expands to the canonical
generic resource key as well, giving all three views one shared HGraph state
dependency. It never aliases ambient `HostWorld`.

This vocabulary describes governed state without pretending that arbitrary
hosted work has become mediated. User-authored effect declarations cannot mint
the governed classes, no production lowering emits them yet, and `HostWorld`
remains the residual umbrella for ambient host effects. The optional grounding
identity is caller-supplied. Underlying identity helpers compare caller pairs;
grounding checks the bound World epoch and membership only, not a live
authoritative nested-generation snapshot. This type/effect foundation is not a
wire format or native Mode 31 and does not implement a distributed Governor,
membership transport, resource registry, `/world` namespace, remote execution,
placement, device assignment, DMA/IOMMU isolation, or current-epoch
enforcement. It passes no G0--G13 gate.

### Nonnormative residual-analysis interpretation

For one concrete validated plan, the finite derived effect summaries contain a
finite set of concrete `ResourceKey` values. Grounding projects their governed
resource and host-resource subsets into separate fields; aliases can overlap
and some key classes belong to neither subset, so this is not a mathematical
partition. `has_residual_host_world()` provides a decidable presence test for
the conservative `HostWorld` cell on each `OperationGrounding`; report-wide
residual status is whether any operation reports it. A future trusted lowering
or effect analysis could justify replacing that umbrella with validated, more
specific keys and thereby refine the *reported* footprint. Merely naming a
specific key does not justify removing `HostWorld` today.

In the associated DRE research model, let `S ~ mu` be a source-program random
variable, let `c` be a deterministic compilation channel, and let `B = c(S)`.
The compiler quantity is `DRE(c, mu) = H_mu(S | B)`: source conditioned on the
emitted artifact. Under that model's semantics-preservation assumptions, the
behavior-only `H_mu(S | beh(S))` is a separate ceiling. Neither quantity is
computed here. `GroundingReport` is not `B` or an entropy estimator; it is a
deterministic projection of a finite validated plan into set-valued diagnostics
and defines no source or workload distribution. A separate execution-context
channel `X -> G = g(X)`, a quantity `H(X | G)`, or a theorem connecting it to
`H_mu(S | B)` remains future research.

This is an engineering analogy, not an information-theoretic result.
`ResourceKey` is not a globally finite universe, a report without `HostWorld`
does not establish complete mediation or replay determinism, and the current
predicate does not measure behavioral entropy. Likewise, the `lost` set in
`Fidelity::Structural` and grounding's ambient sets are both set-valued
residual diagnostics, but they have different elements and transfer rules; the
implementation does not prove that they are duals or the same lattice. Any
quantitative connection still requires defined random variables and
distributions, a soundness argument, and empirical validation. This section is
nonnormative and does not amend the sealed World constitution, the O-Machine
contract, or any G0--G13 gate.

Project inputs use a second, direct planning path because routes are not OIR:

```text
ProjectBundle -> shared ResolvedSelection -> ProjectExecutionPlan -> HGraph
```

`crates/ostadix-api/src/project/plan.rs` binds the logical plan to the exact deterministic bundle
digest and route policy, constructs logically separate materialization branches with
prerequisites, and projects real `MaterializeProject`, `BuildRoute`,
`RunRoute`, `SelectRoute`, and policy-dependent `CompareRouteResults`
operations. Project-specific validation reconstructs the source plan and checks
the exact operation, dependency, effect, and graph projection, so generic graph
well-formedness cannot hide bundle or policy substitution. `olangc` exposes the
result through nonexecuting `ir` and `dot` targets for directory and lifted
bundle inputs.

World PR8-1 adds a strict project-profile `LogicalHGraphV1` normalization over
that exact validated plan/projection. Canonical JSON and a domain-separated
digest bind source, selection policy, operations, typed dependencies, route
facts, declared input/output paths, and complete effect resources without
serializing mutable HGraph execution state. It is an exact-source-bound
projection identity, not a
whitespace-insensitive source-semantic hash: only alternate JSON encodings of
the logical record normalize through decode and canonical re-encoding.
`HostWorld` remains explicit. Logical operation IDs are
planner-local and carry no World identity or authority.

World PR8-2 adds a strict canonical `DeploymentPlanV1` intention layer bound to
that `LogicalHGraphV1` digest. The ordinary hosted constructor is deliberately
unbound: for the policies implemented by `ProjectCoordinator`, it labels
`BuildRoute`, `SelectRoute`, and `CompareRouteResults` as
`HostedCoordinator`, labels `MaterializeProject` and `RunRoute` as
`AmbientHost`, and carries no World, task, provider, or placement identity.
Unsupported hosted policies remain explicitly `Unresolved`; the deployment
record does not silently promote them to executable placements.

Requirements are copied or derived from the exact project bundle: the bundle
digest and bundle-scoped role/path declarations, runtime classes,
executable/evaluator facts, platform and ambient-environment guards, explicit
authority absence, and residual `HostWorld` admission are active compatibility
checks. Bundle environment-overlay key names are recorded separately from
ambient environment requirements. Architecture, package, and failure-domain
fields are schema vocabulary, but the current logical projection leaves them
unconstrained or empty.

A separate constructor can derive a deterministic `ProposedProvider` from a
caller-supplied exact `PlacementSnapshotV1` and one caller-supplied exact
`TaskIdentity` per logical operation. That result is a descriptive proposal,
not current inventory, Governor admission, authority, dispatch, reservation,
or execution. `require_current_world` checks only the referenced World identity
and epoch. The ordinary opt-in executor still uses the hosted-unbound plan. The
separate World-bound entry point consumes the exact snapshot-derived plan and a
`HostedWorldLaunchV1`/`HostedWorldCurrentV1` pair before any workspace or child
exists. The launch carries a caller-supplied coordinator observer and a
coordinator attempt distinct from every operation attempt. It produces a
causally replayed terminal `RuntimeGraphV1` observation with neutral route
settlement and aggregate observed residual `HostWorld`, followed by a
caller-signed, explicitly uncommitted receipt. Receipt placement names the
observer, not the proposed provider, and the receipt does not misuse its package
field for a provider implementation. Mode 32 compares that receipt's complete
unsigned canonical semantics with native `.oc` decoding.

This bounded addition does not implement a `RecoveryPlan`, authenticated
membership, Governor admission or commit, capability/lease issuance,
reservation, remote dispatch, recovery, exactly-once effects, native project
execution, native Ed25519 verification, physical-hardware evidence, G1, or
Workstream A acceptance. G1 remains defined and unpassed.

ProjectExec-A adds an opt-in `ProjectCoordinator` path for one resolved
`Explicit` or `Default` alternative. ProjectExec-B extends that path to serial
ordered `Fallback` and `AnySuccess`: `ReadySchedule` marks their `SelectRoute` input relation as
`OrderedFirstSuccess`, the coordinator retains each attempted alternative
result, and a first success prevents later branches from materializing. With
`O_PROJECT_EXECUTOR=hgraph`, the Project HGraph governs isolated
materialization, typed prerequisite readiness, route execution, and policy
selection. Ordinary route values and successful-completion
tokens are distinct: nonzero settlement publishes its result and conservative
resource successor but cannot release a success-dependent prerequisite edge.
When an alternative settles unsuccessfully, the next alternative starts only
if every route child was guard-skipped or every executed route in that branch,
including successful prerequisites, carries the bundle-bound
`declared_idempotent` contract. That contract is author-declared; it is not
verified effect safety, fencing, journaling, compensation, or exactly-once
evidence. The default `unproven` route contract yields `unproven_effects`
evidence and denies continuation; a failed prerequisite or infrastructure abort
hard-stops. The unsigned trace v5 records this decision and binds both the
canonical `LogicalHGraphV1` schema/digest and the exact canonical
deployment schema/digest. Ordinary plan-aware replay reconstructs the
hosted-unbound deployment artifact; World-bound replay compares the explicitly
supplied snapshot-derived artifact. Both reject substitution. The trace itself
is not an `OWRECEIPT` or attestation. The
compatibility project runtime remains the default when the opt-in is absent.

Materialization and command operations stay fallible and conservatively
read/write `HostWorld`, regardless of untrusted manifest `pure=true` metadata.
The logical alternative branches therefore share conservative ambient/resource
state chains and may be serialized. The ordinary executor remains distinct from
the explicit World-bound hosted-reference adapter. Neither path proves parallel
races or cancellation, retry, independently mediated host worlds, actual remote
placement, Governor authority or commit, capability/lease enforcement,
reservation, recovery, exactly-once effects, native project execution, native
Ed25519 verification, physical hardware, G1, or Workstream A acceptance.
The native product boundary and G0--G13
dependency ladder are fixed in
[`docs/OSTADIX_WORLD.md`](docs/OSTADIX_WORLD.md) and mechanically classified by
[`evidence/world_alpha_gates.toml`](evidence/world_alpha_gates.toml). Hosted
implementations remain useful only under the non-qualifying
[`Hosted World Reference Profile`](docs/HOSTED_WORLD_REFERENCE_PROFILE.md).

Ordinary source sequence is lowered as a predecessor completion-token input.
That dependency is omitted only for direct members of an explicit concurrent
group; for two compiler-verified, read-only O-level `Load` operations outside a
left-to-right `O` region; or when both operations are verified, deterministic,
infallible, resource-free inline renderers from the trusted `html`, `markdown`,
`text`, and `latex` set, each complete structural subtree contains only literal
text and recursively trusted renderers, and neither operation is a child of a
structural `O` sequencing region. Unknown facts preserve sequence. Resource
frontiers can still order members of a concurrent group when their effects
conflict.

`ReadySchedule` derives blockers only from producers of directed operation
inputs. The coordinator durably accepts all outputs atomically at successful
semantic settlement and emits no completion or successor-state token after a
selected failure. The one earlier visibility class is explicitly revocable:
verified-pure admitted-infallible worker outputs may provisionally feed other
safe workers as described below. Deterministic settlement order does not stand
in for effect ordering.
Parallel worker dispatch remains limited to compiler-verified O-scope loads and
source-proven-preparable trees of the four trusted inline renderers. The
coordinator freezes their materialized inputs into owned `PreparedTask`
envelopes and, only when the admitted graph contains `LocalWorker` operations,
submits them to a fixed-size local pool created once for that graph execution
and reused across changing ready frontiers. A coordinator-only graph creates no
pool. Each physical completion wakes the coordinator independently. A
successful compiler-verified pure,
admitted-infallible task may provisionally materialize its value and graph
outputs before the deterministic semantic frontier reaches it, allowing an
infallible worker-only dependent pipeline to refill a free slot. If an earlier
failure wins, those outputs and frame values are revoked and the provisionally
published task is discarded. `NodeFinished` denotes durable settlement, so a
provisionally unlocked dependent may emit `NodeStarted` before the producer's
`NodeFinished`; the admission explanation states this rule. Same-binding loads
share the latest writer frontier and demonstrably execute concurrently.
Because a load may fail without external effects, out-of-order outcomes remain
provisional and settle by serial topological ordinal: fallible loads enter only
as the contiguous unfinished semantic prefix, the lowest-ordinal failure wins,
and every later started outcome is drained and discarded. Infallible
effect-free renderer work may be dispatched outside that prefix. Coordinator
owned operations remain single-owner and the current bounded implementation
does not overlap them with outstanding local-worker tasks. Worker or pool
mechanism failures are infrastructure aborts, not semantic program failures;
started tasks are drained before the trace is finalized. In unwind-capable
builds, caught worker panics use that path. An error returned by an
admitted-infallible adapter is also an infrastructure contract violation rather
than `NodeFailed`. The release profile uses
`panic = "abort"`, so a release worker panic terminates the process and is not
claimed to produce an in-process terminal trace.

## Universal Value System (OValue)

Every value crossing language boundaries is represented as one of these types:

| Type           | Purpose                                             |
|----------------|-----------------------------------------------------|
| `ONull`        | Absence of value                                    |
| `OBool`        | Boolean true/false                                  |
| `ONumber`      | Arbitrary-precision integers, rationals, and floats |
| `OText`        | Text with explicit encoding metadata                |
| `OChar`        | Single Unicode scalar value                         |
| `OHtml`        | HTML fragment                                       |
| `OList`        | Ordered heterogeneous collection                    |
| `OMap`         | String-keyed value mapping                          |
| `OSeq`         | Sequence with source-language shape metadata        |
| `OObject`      | Structural object with deterministic string fields  |
| `OEntriesMap`  | Map with arbitrary OValue keys                      |
| `OSet`         | Set preserving ordered/unordered source intent      |
| `OSymbol`      | Interned symbolic identifier                        |
| `OKeyword`     | Keyword value                                       |
| `OScope`       | Detached lexical binding snapshot                   |
| `OBlob`        | Binary data with MIME type                          |
| `OBytes`       | Structural byte value                               |
| `OGraph`       | Value graph frame for shared identity and cycles    |
| `ONative`      | Language-native capsule with rehydration policy     |
| `OStorePath`   | Nix store path                                      |
| `ONixExpr`     | Unevaluated Nix expression                          |
| `ODerivation`  | Nix derivation                                      |
| `OExpr`        | Unevaluated O expression                            |
| `ORequest`     | Deferred computation with compositional fingerprint |
| `OThunk`       | Captured backend body for Eval requests             |
| `OGroup`       | Explicit execution topology                         |
| `OError`       | Captured failed outcome (produced by `batch`)       |
| `OSystem`      | Live OS/profile reference                           |
| `OCapability`  | Authority-bearing resource handle                   |
| `OSnapshot`    | Persistable captured world state                    |

This table describes the rich hosted `crates/ostadix-api/src/value.rs::OValue` carrier. It is not
the Mode 29 portable allowlist. Conversion into `crates/ostadix-api/src/world/value.rs` is
fallible and rejects authority, capsules, live references, executable or
deferred work, and other effectful hosted variants. The hosted canonical-CBOR
shim protocol is unchanged by `OWVALUE` v1.

The runtime boundary is intentionally split:

- **Pure values** can be cached, replayed, and persisted.
- **Referential values** identify live world entities by handle, not snapshot.
- **Effectful values** carry authority or orchestration meaning and must be
  handled explicitly by schedulers and persistence layers.

OValue lifting and source rendering are distinct boundaries. Lifting maps a
backend result into the tagged OValue carrier. `render_child` projects that
carrier into one consumer language and can be typed, structural, presentational,
or opaque. The exhaustive fidelity matrix is specified in [SPEC.md](SPEC.md)
and implemented by `RenderFidelity`; opaque control and authority values emit
visible markers instead of silently falling through.

Live OCapabilities are not validated from their serialized fields. The hosted
O-core `CapabilityBroker` maps a 256-bit operating-system-random bearer to a
kernel generation-tagged handle in a private session table, then checks kind
and rights before transport. The evaluator uses the same rule for hosted
system activation: a private table maps a live bearer to one authorized
profile. Capability metadata is descriptive only.

Hosted backend effects follow the same rule. `BackendAuthorityBroker` binds a
live bearer to one backend language and a subset of `fs_read`, `fs_write`,
`network`, and `process`. Required adapter rights are embedded in
`BackendInterface`, additional source rights are block attributes, and the
evaluator validates their union before dispatch and before deferred force.
The process registry key includes the full sandbox policy, which prevents a
wider persistent environment from serving a narrower block.

Unprivileged `activate(path[, profile])` constructs a dry activation request.
Mutating `activate(capability, path[, profile])` requires a live
`system_activation` bearer and is checked both at construction and at force
time. Real activation stays on the evaluator thread rather than entering the
autonomous disk-cached scheduler.

## Backend Shims

Each supported language has a shim script in `backends/` that:
- Reads length-prefixed canonical CBOR input from stdin
- Evaluates the expression in the target language
- Writes length-prefixed canonical CBOR output to stdout

Python shims run under an audit policy derived from the validated capability.
On macOS the shim also runs under an operating-system sandbox profile. Bash,
compiled-language, and Nix adapters declare the rights inherent in invoking
their target tool, so those blocks require an explicit host capability even
when the source lists no additional right.

Shims exist for Python, Bash, Shell, Nix, `nix_store`, `nixos_test`, Racket,
Rust, C#, C++, Haskell, Lisp, Common Lisp, SQL, Ruby, MATLAB, Mathematica,
WebAssembly, Java, JavaScript, and OCaml. These are executing adapters for
their local runtimes. `html`, `markdown`, `latex`, `text`, `quote`,
`nix_expr`, and `O` are handled inline without a subprocess. See the backend
table in README.md for runtime requirements.

## Building & Testing

```bash
# Build
cargo build

# Run an example
cargo run -- examples/hello.O backends

# Run tests
cargo test

# Run example smoke tests
bash test_o_lang_examples.sh
```

## Compiler Targets (`olangc`)

`olangc` supports five compilation targets, selected via `--target`:

| Target   | Flag              | Output                              |
|----------|-------------------|-------------------------------------|
| `binary` | `--target binary` | Native ELF/Mach-O binary on disk    |
| `wasm`   | `--target wasm`   | `wasm32-wasip1` module on disk     |
| `script` | `--target script` | In-process execution (no disk file) |
| `ir`     | `--target ir`     | OIR/ExecutionPlan/HGraph, or ProjectExecutionPlan/project HGraph, without execution |
| `dot`    | `--target dot`    | Ordinary or project Graphviz DOT hypergraph on stdout |

**Target A — Binary** (default): creates a temporary Cargo project that
bundles the .O source, runtime, and backend shims, then compiles it with
`cargo build --release`.  The result is a self-contained native binary.

**Target B — WASI**: generates the same hosted runtime project for
`wasm32-wasip1`. Programs remain subject to the subprocess facilities exposed
by their WASI host.

**Target C — Script**: parses and evaluates the .O program directly inside
the `olangc` process.  The evaluator machine code is already loaded into
executable memory as part of the running `olangc` binary — calling it is
semantically equivalent to emitting code into an `mmap`'d executable buffer
and invoking a function pointer.  No intermediate build step or disk binary
is produced.

**Target D — IR**: parses the program with the same front end, lowers the
`ONode` forest to OIR (`crates/ostadix-api/src/ir.rs`), and prints the lowered program to
stdout. A debugging/inspection target — nothing is executed and no output
file is produced. For an ordinary `.O` file, `--explain-schedule` additionally
solves the graph, compiles evidence-bound admission, and prints its digests,
provenance, blockers, retained sequence reasons, and legal static waves. A
`runtime-snapshot kind=inspection` line makes explicit that this certificate
preview is not interchangeable with an evaluator's dispatch-time execution
snapshot. A separate advisory `ScheduleRealizability` marker samples the
inspection host and derives the default worker count as
`min(available_parallelism, admitted_max_local_worker_wave_width).max(1)`;
`olangc FILE --target ir --explain-schedule --workers N` shows an explicit
override without clamping it to the reported host value or static width. A
failed host `available_parallelism` query is represented conservatively as one.
The marker schema ID is `oexec.realizability/v1`; its total width includes all
operations in the widest static wave, whereas its local-worker width counts
only operations that consume pool slots. `worker-count-covers-static-wave` is
`not-applicable` for zero local-worker width and otherwise reports the
arithmetic comparison between selected count and local-worker width. Even
`yes` leaves `execution-realizable=unknown`: the marker is outside the admission
digest and establishes neither simultaneous dispatch, CPU/memory fit,
external-runtime readiness, a placement lease, nor observed overlap. Those
waves describe static legality, not dynamic dispatch groups or completion
order. A directory or
lifted project instead constructs its typed,
exact-provenance
`ProjectExecutionPlan` and HGraph without running any route; project admission
explanation is deferred.

**Target E — Dot**: parses and lowers to OIR, then builds the full
`HGraph` hypergraph (`crates/ostadix-api/src/hgraph/`) from that OIR, runs the type solver, and
serialises the result as a Graphviz DOT digraph on stdout. Ordinary values,
resource versions, actor-state versions, and completion/control values have
distinct styles. Executable and constraint hyperedges are explicit vertices,
so input-to-operation and operation-to-output port direction remains visible.
For a directory or lifted project it renders the separately validated project
HGraph. Nothing is executed and no output file is produced.

```bash
# Compile to a binary (Target A)
cargo run --bin olangc -- examples/hello.O -o hello

# Compile to WASI (Target B)
cargo run --bin olangc -- examples/hello.O --target wasm -o hello.wasm

# Execute in-process (Target C)
cargo run --bin olangc -- examples/hello.O --target script

# Dump the lowered OIR (Target D)
cargo run --bin olangc -- examples/hello.O --target ir

# Emit Graphviz DOT hypergraph (Target E)
cargo run --bin olangc -- examples/hello.O --target dot

# Render to PNG via Graphviz
cargo run --bin olangc -- examples/hello.O --target dot | dot -Tpng -o graph.png
```

## Implementations

| Edition | Directory | Status     |
|---------|-----------|------------|
| Rust engine | `crates/ostadix-api/` | **Active** |
| Rust CLI/compatibility shell | `src/` | **Active** |
| C17     | `c_cpp/`  | Complete   |
| Python  | `o_lang/` | Reference  |
