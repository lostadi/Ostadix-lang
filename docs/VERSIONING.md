# Ostadix versioning and compatibility

Ostadix deliberately has more than one version axis. A package release, Rust
compiler floor, wire protocol, admission schema, backend catalog, and
information record answer different compatibility questions and may advance
independently. Matching numbers across axes are coincidence, not compatibility.

Run `O version --json` to inspect the runtime coordinates compiled into the
current interpreter. The source constants named below remain authoritative for
axes not present in that report. Every version report is descriptive: it does
not prove that an external runtime is installed, that a placement is
authorized, that an information projection is fresh, or that a World is live.

## Version axes

| Axis | Current coordinate | Source authority | Compatibility meaning |
|---|---|---|---|
| Rust packages | `0.4.0` in the current manifests | root and `crates/ostadix-api/Cargo.toml` `package.version` | Package coordinate only. The immutable v0.3.0 tag has the historical narrow façade; the post-tag Unreleased 0.4 line makes `ostadix-api` the full engine. |
| Minimum Rust | `1.93.1` | `Cargo.toml` `package.rust-version` | Lowest compiler supported by the MSRV contract. This is not the package version or release compiler. |
| Release toolchain | `1.97.1` | `rust-toolchain.toml` `toolchain.channel` | Pinned compiler for release, formatting, Clippy, and generated-runtime evidence. This may advance without changing the MSRV. |
| Execution intent | `oexec.execution-intent/v1` | `crates/ostadix-api/src/evidence/intent.rs` `EXECUTION_INTENT_SCHEMA_V1` | Stable, authority-free identity of exact source and analyzed semantics. |
| Operation and realization descriptions | `ostadix.operation-contract/v1`, `ostadix.operation-interface/v1`, `ostadix.realization-descriptor/v1`, `ostadix.realization-set/v1` | the four `*_SCHEMA_V1` constants in `crates/ostadix-api/src/computation_core.rs` | Experimental canonical descriptive records. Their cross-check establishes referential consistency only, never planning, selection, equivalence, placement, execution, recovery, or authority. |
| Operation planning and observation | planning records V1; logical, deployment, and runtime graphs V2; marked-project bridge V1 | the nine record constants in `crates/ostadix-api/src/computation/realization_plan.rs` and three `OPERATION_*_SCHEMA_V1` bridge constants in `src/bin/o-cli.rs` | Experimental canonical records, deterministic single-operation static ranking, exact marked-project joins, and current-binary observation reconstruction. Selection is not live eligibility, placement, execution authority, or recovery. |
| Solved executable graph | `ostadix-solved-executable-hgraph/v2` | `crates/ostadix-api/src/evidence/analyze.rs` `SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V2`; `graph_sha256_v2` | Current Graph V2 identity binds the complete canonical `FidelityAssessmentV2`. Graph V1 remains frozen for intent and archival V5 inspection. |
| Evidence | `oexec.evidence/v6` | `crates/ostadix-api/src/evidence/fact.rs` `EVIDENCE_SCHEMA_V6`; `ANALYZER_ID_V6` = `ostadix-oir-evidence-compiler/v6` | Current typed pre-execution evidence vocabulary; evidence is not admission. |
| Admission | `oexec.admission/v6` | `crates/ostadix-api/src/evidence/fact.rs` `ADMISSION_SCHEMA_V6` | Current process-local admitted-execution contract accepted by coordinator/evaluator. |
| Schedule explanation | `oexec.schedule-explanation/v2`; `oexec.admission-why/v2` | `SCHEDULE_EXPLANATION_SCHEMA_V2`; `SCHEDULE_WHY_SCHEMA_V2` | Current whole-admission and focused typed V6 inspection projections. V1 forms are archival. |
| Placement admission | `ostadix/placement-admission/v2` | `crates/ostadix-api/src/evidence/admit.rs` `PLACEMENT_ADMISSION_DIGEST_DOMAIN_V2` | Current process-portable V6/Graph V2 semantic coordinate consumed by freshly prepared placement V2 authority. |
| Backend catalog | `ostadix.backend-catalog/v6` | `crates/ostadix-api/src/backend_catalog.inc.rs` `current_schema` | Canonical backend specification and implementation-identity projection. V6 retains frozen V5 fields and adds two ordered `wasm-tools` WebAssembly runtime alternatives. |
| Backend morphism | `ostadix.backend-morphism/v1` | `crates/ostadix-api/src/backend_morphism.rs` `BACKEND_MORPHISM_SCHEMA_V1` | Experimental shadow-only crossing kernel. Catalog V5 introduced the profile assignment and Catalog V6 retains it, but does not enforce its assessment through evidence, admission, placement, or dispatch. |
| Hosted transport | `ostadix.hosted-transport/v1`, `ostadix.hosted-transport/v2` | `crates/ostadix-api/src/hosted_remote/protocol.rs` `HOSTED_PROTOCOL_V1`; `crates/ostadix-api/src/hosted_remote/v2/protocol.rs` `HOSTED_PROTOCOL_V2`; `crates/ostadix-api/src/hosted_remote/v2/store.rs` `HOSTED_STATE_AUTHORITY_SCHEMA_V1` = `ostadix.hosted-state-authority/v1` | Frozen single-operation V1 and opt-in durable-session V2 wire contracts. Package 0.3 and later roots bind their durable state to current Graph V2/V6/placement V2 and reject older journals without migration. |
| Pure execution records | `ostadix.oir-execution-capsule/v1`, `ostadix.oir-execution-candidate/v1` | `crates/ostadix-api/src/execution_fabric/protocol.rs` `EXECUTION_CAPSULE_SCHEMA_V1`, `EXECUTION_CANDIDATE_SCHEMA_V1` | Frozen M2 authority-free capsule and provisional-candidate records. M3 nests their exact canonical bytes and does not add node, transport, placement, TLS, or HGraph fields. |
| Authenticated execution Fabric | ALPN `ostadix-execution-fabric/1`; request/response/submission/source-closure V1; placement lease V3; terminal receipt V1 | `crates/ostadix-api/src/hosted_remote/tls.rs` `EXECUTION_FABRIC_TLS_ALPN_V1`; `crates/ostadix-api/src/execution_fabric_authority/protocol.rs` `FABRIC_*` constants | Opt-in M3 transport and authority envelopes for the frozen pure profile. They authorize one exact provider attempt and return a provisional result; they confer no graph mutation, publication, or settlement authority. |
| Placement milestone | Hosted Placement V6 | `docs/HOSTED_PLACEMENT_V6.md` | A placement/evidence milestone name, not a source-level schema constant and not an automatic conversion from archival Admission V5. |
| World wire family | V1 | `crates/ostadix-api/src/world/protocol.rs` `WORLD_SCHEMA_V1`; codec constants under `crates/ostadix-api/src/world/` | Offline World record, identity, value, and receipt codecs. These coordinates do not establish a live World. |
| Information kernel family | V1 | `INFORMATION_SCHEMA_V1` and record constants under `crates/ostadix-api/src/information/` | Authority-free, immutable information identities, deltas, projections, and receipts. Each record validates its own exact schema. |
| Information native bridge | V1 | `INFORMATION_BRIDGE_SCHEMA_V1` and record constants under `crates/ostadix-api/src/information_bridge/mod.rs` | Eight explicit, lossy, authority-free native metadata projections. This leaf family is independent of native record versions and does not replace their identities. |

“V6” in Hosted Placement V6 names the placement milestone and placement-evidence
model; it is not a promise that every schema or transport is numerically version
6. It is also not the same coordinate as the current local
`oexec.evidence/v6` and `oexec.admission/v6` APIs below.

### Current and archival local admission coordinates

Package `0.4.0` retains the fidelity-preserving path through the current
runtime and its explicit archival entry points while changing crate ownership.
At the v0.3.0 tag,
`ostadix-api` was an owned narrow `Runtime` wrapper over the root package. The
post-tag 0.4 tree reverses that ownership: `ostadix-api` owns the complete
runtime graph and the root `o-lang` crate explicitly reexports its historical
module paths. Future package publication must therefore publish and verify the
engine first, then publish the exact-version CLI/compatibility shell.

The preceding `0.2.0` package exposed the V6 path additively without changing
dispatch. Package `0.3.0` atomically makes these coordinates current:

| Explicit coordinate | Source authority | Meaning |
|---|---|---|
| Graph V2 / `ostadix-solved-executable-hgraph/v2` | `crates/ostadix-api/src/evidence/analyze.rs` `graph_sha256_v2` | Frozen Graph V1 fields plus the complete canonical `FidelityAssessmentV2`; absent and present assessments are distinct. |
| `oexec.evidence/v6` / `ostadix-evidence-bundle/v6` | `EVIDENCE_SCHEMA_V6`; `evidence_bundle_sha256_v6` | Typed per-operation fidelity assessment bound to Graph V2 and the current Catalog V6 projection. |
| `oexec.admission/v6` / `ostadix-execution-admission/v6` | `ADMISSION_SCHEMA_V6`; `admit_execution_v6` | Revalidated local V6 admission accepted by the current coordinator and evaluator. |
| `oexec.admission-why/v2` | `SCHEDULE_WHY_SCHEMA_V2` | Inspection-only V6 why-view retaining typed fidelity evidence. |
| `oexec.schedule-explanation/v2` | `SCHEDULE_EXPLANATION_SCHEMA_V2` | Whole-admission JSON projection whose embedded admission is V6; V1 bytes remain archival. |
| `ostadix/placement-admission/v2` | `PLACEMENT_ADMISSION_DIGEST_DOMAIN_V2` | Process-portable semantic digest over V6 schemas/analyzer, Graph V2 identities, current Catalog V6 projection, and policy. Fresh `PlacementFragmentBindingsV2` binds this digest. |

The unversioned `analyze_execution` and `admit_execution`, `AdmittedExecution`,
uppercase `O`, evaluator/coordinator, CLI JSON, version report, and MCP behavior
now mean V6/Graph V2. Current prepared execution uses
`PlacementFragmentBindingsV2` and `PreparedPlacementFragmentV2`. The public V1
fragment/binding vocabulary and `AdmittedExecutionV5` are inspection-only and
cannot enter current execution or authorization. `ExecutionIntentV1` remains
Graph V1 while binding the current Catalog V6 projection, and execution of a
matching intent always performs fresh V6 analysis/admission. There is no
V5-to-V6 conversion, relabel, prepared-fragment uplift, lease migration, or
journal migration.

The frozen archival coordinates remain explicit: `EVIDENCE_SCHEMA_V5`
(`oexec.evidence/v5`), `ADMISSION_SCHEMA_V5` (`oexec.admission/v5`),
`ANALYZER_ID_V5`,
`SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V1`/`graph_sha256_v1`,
`EVIDENCE_BUNDLE_DIGEST_DOMAIN_V5`,
`EXECUTION_ADMISSION_DIGEST_DOMAIN_V5`,
`PLACEMENT_ADMISSION_DIGEST_DOMAIN_V1` (`ostadix/placement-admission/v1`),
`SCHEDULE_EXPLANATION_SCHEMA_V1` (`oexec.schedule-explanation/v1`), and
`SCHEDULE_WHY_SCHEMA_V1` (`oexec.admission-why/v1`). They support compatibility
inspection and golden verification only.

### Operation-realization and planning coordinates

The four source coordinates are independent records, not one interchangeable
bundle version:

| Record | Coordinate | Source constant |
|---|---|---|
| Contract | `ostadix.operation-contract/v1` | `OPERATION_CONTRACT_SCHEMA_V1` |
| Interface | `ostadix.operation-interface/v1` | `OPERATION_INTERFACE_SCHEMA_V1` |
| Realization descriptor | `ostadix.realization-descriptor/v1` | `REALIZATION_DESCRIPTOR_SCHEMA_V1` |
| Realization set | `ostadix.realization-set/v1` | `REALIZATION_SET_SCHEMA_V1` |

All four constants and their canonical encoders live in
`crates/ostadix-api/src/computation_core.rs`. Each record has its own digest
domain. Changing the serialized or validation meaning of one requires a new
coordinate for that record; matching `v1` suffixes do not permit the others to
be silently changed or uplifted.

The additive authority-free planning family has these independent coordinates:

| Record | Coordinate | Source constant |
|---|---|---|
| Physical representation | `ostadix.physical-representation/v1` | `PHYSICAL_REPRESENTATION_SCHEMA_V1` |
| Transfer plan | `ostadix.transfer-plan/v1` | `TRANSFER_PLAN_SCHEMA_V1` |
| Cost profile | `ostadix.cost-profile/v1` | `COST_PROFILE_SCHEMA_V1` |
| Objective | `ostadix.objective/v1` | `OBJECTIVE_SCHEMA_V1` |
| Logical operation HGraph | `ostadix.logical-hgraph/v2` | `LOGICAL_HGRAPH_SCHEMA_V2` |
| Descriptive deployment plan | `ostadix.deployment-plan/v2` | `DEPLOYMENT_PLAN_SCHEMA_V2` |
| Runtime observation graph | `ostadix.runtime-graph/v2` | `RUNTIME_GRAPH_SCHEMA_V2` |
| Recovery plan | `ostadix.recovery-plan/v1` | `RECOVERY_PLAN_SCHEMA_V1` |
| Operation-planning request | `ostadix.operation-planning-request/v1` | `OPERATION_PLANNING_REQUEST_SCHEMA_V1` |

All nine constants, canonical encoders, and record-specific digest domains live
in `crates/ostadix-api/src/computation/realization_plan.rs`. The V2 suffixes on
`LogicalHGraphV2`, `DeploymentPlanV2`, and `RuntimeGraphV2` distinguish these
general operation-schema shapes from project-specific V1 records and from the
separate solved-executable-HGraph coordinate. Numerical agreement across those
families conveys no conversion or compatibility.

The marked-project CLI adds three independent bridge/report coordinates:

| Surface | Coordinate | Source constant |
|---|---|---|
| Manifest route-pipeline projection | `ostadix.project-route-pipeline/v1` | `OPERATION_ROUTE_PIPELINE_SCHEMA_V1` in `src/bin/o-cli.rs` |
| Runtime observation binding | `ostadix.operation-runtime-binding/v1` | `OPERATION_RUNTIME_BINDING_SCHEMA_V1` in `src/bin/o-cli.rs` |
| Operation command error envelope | `ostadix.operation-command-error/v1` | `OPERATION_COMMAND_ERROR_SCHEMA_V1` in `src/bin/o-cli.rs` |

The route-pipeline coordinate identifies the deterministic serialized projection
of the exact manifest route fields; the descriptor's execution-pipeline content
reference must match it. The descriptor's implementation digest independently
binds a captured regular non-symlink file, and the manifest tuple key binds the
exact cost-profile ID. These joins do not prove that the route loads the named
implementation, behavioral equivalence, or physical target execution.

The runtime-binding coordinate identifies content that includes an exact
`RunContentRefV1`, unchanged bundle digest, current recomputed
`OperationPlanningRequestV1`/`DeploymentPlanV2` IDs and selected tuple, exact
route-pipeline identity, and recorded-versus-recomputed project-route
logical-HGraph/deployment identities. `RunRecordV1` persisted the bundle, route,
route result, and project-route plan identities, but it did not persist the
operation planning-request ID, operation DeploymentPlan V2 ID, or selected
candidate tuple. The runtime binding is therefore current-binary consistency
evidence, not historical persistence of those operation-plan coordinates.

`REQUIREMENT_FOOTPRINT_CONTENT_SCHEMA_V1` is
`ostadix.placement.requirement-footprint/v1`. It labels a raw canonical-content
join to the existing placement footprint; it is not a typed planning-record ID
or an authority coordinate.

`OComputationManifestV1` and `FacetKindV1` were first introduced after v0.3.0
on the unreleased 0.4 line. The facet-kind spellings `operation_contract`,
`operation_interface`, `realization_descriptor`, `realization_set`,
`physical_representation`, `transfer_plan`, `cost_profile`, `objective`, and
`recovery_plan` are part of that manifest's initial V1 freeze before its first
package release. Existing pinned `OComputationManifestV1` byte vectors remain
unchanged. Older unreleased binaries may reject newly authored manifests that
carry these spellings; this is not a claim of backwards reader compatibility.

`ostadix.operation-inspection/v1`, `ostadix.operation-verification/v1`,
`ostadix.operation-project-description/v1`,
`ostadix.operation-realization-catalog/v1`,
`ostadix.operation-plan-summary/v1`, `ostadix.operation-observation/v1`, and
`ostadix.operation-replan/v1` version successful CLI output envelopes, not the
input records or an authority protocol. `ostadix.operation-command-error/v1`
separately versions their JSON error envelope. Inspection and verification
retain their referential-only meaning. Planning envelopes report deterministic
static selection. The observation envelope carries a `RuntimeGraphV2` bound to
a content-verified retained record plus the current recomputed operation and
project-route plans; it is not a historical operation-plan receipt. The
replanning envelope carries a recomputed `DeploymentPlanV2` and carries a
`RecoveryPlanV1` only for a failed source observation whose replacement
selection changes. Its ambient target alternatives are descriptive/static, not
distinct observed failure domains. Recovery planning and recovery execution are
separate; the command does not dispatch the alternative. These envelopes create
no World state, recovery action, or dispatch authority. The package version
report does not currently add these experimental coordinates; the source
constants, `docs/OPERATION_REALIZATION_V1.md`, and
`docs/OPERATION_PLANNING_V1.md` are authoritative.

### Attribution-rewrite provenance map

`evidence/attribution-rewrite-2026-09-03.commit-map` is the exact
`git-filter-repo` commit map generated by the 2026-09-03 attribution-only
rewrite of `master`. Its 598 rows cover the old commits reachable from the
pre-feature `master` lineage and their corresponding commits on rewritten
`master`; every mapped pair was verified to have the same Git tree. The map is
SHA-256 sealed by both the World evidence validator and source-release builder.

The annotated `v0.2.0` and `v0.3.0` tags remain immutable and were not moved
onto the rewritten lineage, so their histories are intentionally disconnected
from rewritten `master`. This artifact is a bounded provenance bridge, not a
universal repository-history map: it does not claim to cover every older
side-lineage or narrative commit coordinate, nor commits reachable only from a
remaining remote branch.

### World V1 coordinates

The World row is a family, not one interchangeable integer. Its current source
constants are `WORLD_SCHEMA_V1` in `crates/ostadix-api/src/world/protocol.rs`,
`WORLD_WIRE_CODEC_VERSION` in `crates/ostadix-api/src/world/codec.rs`, `IDENTITY_WIRE_VERSION` in
`crates/ostadix-api/src/world/identity_wire.rs`, `OVALUE_WIRE_SCHEMA_V1` in
`crates/ostadix-api/src/world/value_codec.rs`, and `WORLD_RECEIPT_SCHEMA_V1` in
`crates/ostadix-api/src/world/receipt_codec.rs`. All currently carry coordinate `1`, but a future
change to one does not silently change the others.

### Information-kernel V1 coordinates

`INFORMATION_SCHEMA_V1` in `crates/ostadix-api/src/information/mod.rs` names the family. Individual
records retain their own coordinates:

| Record | Coordinate | Source constant |
|---|---|---|
| Family | `ostadix.information/v1` | `crates/ostadix-api/src/information/mod.rs` `INFORMATION_SCHEMA_V1` |
| Atom | `ostadix.info-atom/v1` | `crates/ostadix-api/src/information/model.rs` `INFORMATION_ATOM_SCHEMA_V1` |
| Entity | `ostadix.info-entity/v1` | `crates/ostadix-api/src/information/model.rs` `ENTITY_DESCRIPTOR_SCHEMA_V1` |
| Snapshot | `ostadix.info-snapshot/v1` | `crates/ostadix-api/src/information/root.rs` `INFORMATION_SNAPSHOT_SCHEMA_V1` |
| Revision | `ostadix.info-revision/v1` | `crates/ostadix-api/src/information/root.rs` `INFORMATION_REVISION_SCHEMA_V1` |
| Delta | `ostadix.info-delta/v1` | `crates/ostadix-api/src/information/delta.rs` `INFORMATION_DELTA_SCHEMA_V1` |
| Projection receipt | `ostadix.projection-receipt/v1` | `crates/ostadix-api/src/information/projection.rs` `PROJECTION_RECEIPT_SCHEMA_V1` |
| Offline delta pack | `ostadix.info-delta-pack/v1` | `crates/ostadix-api/src/information/exchange.rs` `INFORMATION_DELTA_PACK_SCHEMA_V1` |
| Signed offline delta pack | `ostadix.signed-info-delta-pack/v1` | `crates/ostadix-api/src/information/exchange.rs` `SIGNED_INFORMATION_DELTA_PACK_SCHEMA_V1` |
| Decision receipt | `ostadix.info-decision/v1` | `crates/ostadix-api/src/information/decision.rs` `DECISION_RECEIPT_SCHEMA_V1` |
| Observation | `ostadix.info-observation/v1` | `crates/ostadix-api/src/information/decision.rs` `OBSERVATION_RECORD_SCHEMA_V1` |

`LossContractV1` is a typed component of the projection receipt rather than an
independently labeled wire schema. Changing its serialized meaning therefore
requires advancing the containing receipt coordinate.

### Information-bridge V1 coordinates

The bridge family is `ostadix.information-bridge/v1` with media type
`application/cbor`. Its eight exact record coordinates are:

| Record | Coordinate |
|---|---|
| Parsed document | `ostadix.info-bridge-parsed-document/v1` |
| Caller-public scalar | `ostadix.info-bridge-public-value/v1` |
| HGraph metadata | `ostadix.info-bridge-hgraph/v1` |
| Evidence metadata | `ostadix.info-bridge-evidence/v1` |
| Registry profile metadata | `ostadix.info-bridge-registry-profile/v1` |
| World receipt metadata | `ostadix.info-bridge-world-receipt/v1` |
| Project graph metadata | `ostadix.info-bridge-project-graph/v1` |
| Hosted journal metadata | `ostadix.info-bridge-hosted-journal/v1` |

HGraph and Evidence use separate
`HGRAPH_METADATA_PROJECTION_DIGEST_DOMAIN_V1` and
`EVIDENCE_METADATA_PROJECTION_DIGEST_DOMAIN_V1` domains over only their
exported allowlists. Registry node, Hosted session, and Hosted entry/link
identities use their separate bridge domains. These are lossy projection or
equality coordinates, not the Graph V2/Evidence V6/native journal identities,
privacy primitives, or authority. The package version report remains unchanged:
these experimental source constants are authoritative, and adding a report
field would require a separately versioned report-schema change. The advanced
independent `ostadix_api::api` exports the bridge; generated AOT runtimes do
not. The `o_lang::api` path is a compatibility reexport of that same module.

These V1 records describe canonical, provenance-bearing facts and certified
projections. A matching root or receipt is not execution authority, placement
admission, runtime availability, freshness beyond its declared preconditions,
or World membership. Physical compression, replication, and object location
are outside the logical schema identity unless an individual record explicitly
commits to them.

## Compatibility rules

1. Wire and signed-evidence decoders validate their exact schema. No version is
   silently relabeled or uplifted.
2. A stable execution intent proves sameness of modeled input, not authority,
   current runtime availability, or reusable admission.
3. Backend-catalog generation changes invalidate identities derived from the
   older catalog. Regenerate profiles, warrants, and short-lived evidence.
4. Hosted V1 and V2 are separate protocols. Supporting V2 does not mutate the
   frozen V1 contract.
5. World wire versions cover canonical offline records; they do not claim a
   live World transport or Governor service.
6. Information-kernel schemas version logical facts, roots, deltas, loss
   contracts, and receipts. They do not version a storage engine and do not
   mint or replace evidence, admission, placement, or World authority.
7. Backend-morphism V1 remains an inspection-only profile family. Catalog V5
   introduced the selected optional profile and Catalog V6 retains it. The explicit Graph V2/Evidence V6 path
   preserves typed solver assessments, but does not itself enforce morphism
   profiles or silently strengthen or narrow current V6 semantics.
8. At v0.3.0, `ostadix-api` was the narrow stable embedding facade. In the
   post-tag Unreleased tree it is the full runtime engine and owns the advanced
   modules. Root `o_lang::<module>` paths are explicit compatibility reexports
   of those same nominal types, not separately compiled implementations.
9. Package-0.3-and-later hosted state roots carry
   `HOSTED_STATE_AUTHORITY_SCHEMA_V1`. A root containing earlier journals but
   no exact marker is rejected without mutation; there is no journal uplift.
10. Information Atom V1 canonical bytes and IDs remain frozen. Information
    Provenance V2 is an additive sidecar keyed by `AtomIdV1`; a raw sidecar is
    descriptive, while verified provenance requires an opaque admitted handle
    returned after trusted contextual reanalysis.
11. The M2 execution capsule and candidate remain frozen, and Hosted
    `PlacementLeaseV2` retains its signed Hosted V2 meaning. Fabric
    `PlacementLeaseV3` is additive and authorizes only the exact M3 pure
    attempt. Fabric uses the exact opt-in `ostadix-execution-fabric/1` ALPN;
    unknown or malformed traffic is never sniffed, uplifted, or routed to
    Hosted or Mesh fallback. The provider's node-local `FabricAttemptLedgerV1`
    is replay/fencing state, not the deferred M4 coordinator journal. The M3
    delivery label is neither a wire-schema version nor the separate O-core M3
    IPC milestone.
12. Operation-realization V1 records are canonical declarations. A successful
    decode or exact cross-record verification cannot be reinterpreted as a
    plan, winner selection, behavioral-equivalence proof, authenticated
    evidence, target eligibility, placement, execution, recovery, admission,
    capability, lease, or World authority.
13. Operation-planning records use independent coordinates and typed identities.
    Static rankability is not live eligibility. A deployment selection, runtime
    observation, or verified recovery closure cannot be uplifted into admission,
    reservation, lease, dispatch, checkpoint restoration, effect fencing, or
    World authority. A current-binary observation does not retrospectively add
    an operation plan or selected tuple to `RunRecordV1`; descriptive target
    alternatives do not establish distinct failure domains; and recovery
    planning does not execute recovery. Replanning a successful retained run is
    not recovery.

The frozen M2 records, additive Fabric authority, and exact ALPN routes use
these exported coordinates:

| Constant | Literal value | Defining source |
|---|---|---|
| `EXECUTION_CAPSULE_SCHEMA_V1` | `ostadix.oir-execution-capsule/v1` | `crates/ostadix-api/src/execution_fabric/protocol.rs` |
| `EXECUTION_CANDIDATE_SCHEMA_V1` | `ostadix.oir-execution-candidate/v1` | `crates/ostadix-api/src/execution_fabric/protocol.rs` |
| `FABRIC_REQUEST_SCHEMA_V1` | `ostadix.execution-fabric-request/v1` | `crates/ostadix-api/src/execution_fabric_authority/protocol.rs` |
| `FABRIC_RESPONSE_SCHEMA_V1` | `ostadix.execution-fabric-response/v1` | `crates/ostadix-api/src/execution_fabric_authority/protocol.rs` |
| `FABRIC_SUBMISSION_SCHEMA_V1` | `ostadix.execution-fabric-submission/v1` | `crates/ostadix-api/src/execution_fabric_authority/protocol.rs` |
| `FABRIC_SOURCE_CLOSURE_SCHEMA_V1` | `ostadix.execution-source-closure/v1` | `crates/ostadix-api/src/execution_fabric_authority/protocol.rs` |
| `FABRIC_SOURCE_CLOSURE_DIALECT_V1` | `ostadix-source-closure/v1` | `crates/ostadix-api/src/execution_fabric_authority/protocol.rs` |
| `FABRIC_PLACEMENT_LEASE_SCHEMA_V3` | `ostadix.execution-placement-lease/v3` | `crates/ostadix-api/src/execution_fabric_authority/protocol.rs` |
| `FABRIC_SIGNED_LEASE_SCHEMA_V3` | `ostadix.signed-execution-lease/v3` | `crates/ostadix-api/src/execution_fabric_authority/protocol.rs` |
| `FABRIC_TERMINAL_RECEIPT_SCHEMA_V1` | `ostadix.execution-fabric-terminal-receipt/v1` | `crates/ostadix-api/src/execution_fabric_authority/protocol.rs` |
| `FABRIC_SIGNED_TERMINAL_RECEIPT_SCHEMA_V1` | `ostadix.signed-execution-fabric-terminal-receipt/v1` | `crates/ostadix-api/src/execution_fabric_authority/protocol.rs` |
| `HOSTED_TLS_ALPN_V1` | `ostadix-hosted/1` | `crates/ostadix-api/src/hosted_remote/tls.rs` |
| `HOSTED_TLS_ALPN_V2` | `ostadix-hosted/2` | `crates/ostadix-api/src/hosted_remote/tls.rs` |
| `HOSTED_TLS_ALPN_MESH_V1` | `ostadix-mesh/1` | `crates/ostadix-api/src/hosted_remote/tls.rs` |
| `EXECUTION_FABRIC_TLS_ALPN_V1` | `ostadix-execution-fabric/1` | `crates/ostadix-api/src/hosted_remote/tls.rs` |

Execution Intent V1 continues to bind the solved graph V1 identity and the
process's current backend-catalog projection. The V6 catalog rollover therefore
changes that catalog binding without changing the graph algorithm or schema.
Archival V5 and V4 whole/per-specification helpers remain available for inspection but
never authorize a current Catalog V6-backed placement V2.

## Package 0.3 source breaks

- Unversioned analysis/admission and `AdmittedExecution` now use
  `EvidenceBundleV6`/`AdmittedExecutionV6`; `NodeEvidence` now aliases
  `NodeEvidenceV2`.
- Coordinator, evaluator, CLI explanation, and `last_execution_admission()`
  expose V6/Why V2 rather than V5/Why V1.
- `prepare_placement_fragment()` returns `PreparedPlacementFragmentV2`, and
  current execution accepts only that type. `PlacementFragmentBindingsV1` and
  `PreparedPlacementFragmentV1` are inspection-only.
- `PlacementAuthorizationContextV2::prepared_fragment` now contains
  `PlacementFragmentBindingsV2`. This deliberately breaks source compatibility
  for exhaustive Hosted V2 literals while retaining the hosted wire protocol
  coordinate; old signed V1 placement admission digests fail current lease
  validation.
- The root manifest is now a two-member workspace. Commands targeting a root
  binary must select `--package o-lang`; ordinary unscoped workspace gates
  compile both default members.
- The new publishable `ostadix-api = 0.3.0` facade depends exactly on
  `o-lang = 0.3.0`, so releases publish the root crate before the facade.
- `ParsedDocumentV1.nodes` is now private before the 0.3 tag. Use `nodes()` for
  borrowed access or `into_nodes()` for owned extraction. Its derived equality
  also includes parser-captured source SHA-256 and length. The stable
  `ostadix-api` facade did not expose this type and remains source-unchanged.

## Information Provenance V2

`ostadix.info-provenance/v2` separates acquisition origin, assurance, claim
standing, and recoverability instead of storing them as one caller-selected
modality. Its recovery result is relative to an explicit question, equivalence
contract, domain, and trusted context. A digest reference names bytes; it does
not by itself discharge execution fidelity or freshness.

The initial execution adapter image-produces only `Derivation`
classifications from an opaque V6 admission and a signature-verified World
receipt. It binds the admission digest and terminal T2 output, but
conservatively leaves unresolved
producer authentication, signer authorization, receipt currentness, exact
plan-node attribution, execution/effect fidelity, and morphism fidelity. The
signature proof is cryptographic verification under a caller-supplied resolver,
not proof that the signer was authorized for this claim. This is a new V2
coordinate and never silently upgrades, relabels, or rewrites V1 atoms.

## Unreleased engine-ownership boundary

After the immutable v0.3.0 tag, runtime ownership moves atomically into
`ostadix-api`. The engine package owns the implementation sources, direct
runtime dependencies, embedded shim assets, benchmark/native test assets, and
the AOT source bundle. The root `o-lang` package owns the binaries and explicitly
reexports the historical public module set through one exact-version dependency.

This preserves nominal type identity and source imports such as
`o_lang::value::OValue`, but it changes the defining crate visible in reflected
diagnostics such as `std::any::type_name` and rustdoc links. It also reverses
registry publication order to `ostadix-api` first and `o-lang` second. These
source-contract changes use the synchronized Unreleased `0.4.0` package
coordinate and must never be published by moving or replacing the v0.3.0 tag.

## Changing a coordinate

Any version change must update its canonical constant, compatibility tests,
machine-readable version report where that axis is represented, release
documentation, generated/AOT source closure where applicable, and
mutation/rejection tests for the prior identity. Package, MSRV, toolchain, and
citation metadata are validated separately so a toolchain upgrade does not
masquerade as a package or protocol release. A new information record schema
must also state its canonical identity inputs, loss or invalidation behavior,
and whether older records remain inspectable, projectable, or admissible.
