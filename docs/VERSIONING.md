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
| Solved executable graph | `ostadix-solved-executable-hgraph/v2` | `crates/ostadix-api/src/evidence/analyze.rs` `SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V2`; `graph_sha256_v2` | Current Graph V2 identity binds the complete canonical `FidelityAssessmentV2`. Graph V1 remains frozen for intent and archival V5 inspection. |
| Evidence | `oexec.evidence/v6` | `crates/ostadix-api/src/evidence/fact.rs` `EVIDENCE_SCHEMA_V6`; `ANALYZER_ID_V6` = `ostadix-oir-evidence-compiler/v6` | Current typed pre-execution evidence vocabulary; evidence is not admission. |
| Admission | `oexec.admission/v6` | `crates/ostadix-api/src/evidence/fact.rs` `ADMISSION_SCHEMA_V6` | Current process-local admitted-execution contract accepted by coordinator/evaluator. |
| Schedule explanation | `oexec.schedule-explanation/v2`; `oexec.admission-why/v2` | `SCHEDULE_EXPLANATION_SCHEMA_V2`; `SCHEDULE_WHY_SCHEMA_V2` | Current whole-admission and focused typed V6 inspection projections. V1 forms are archival. |
| Placement admission | `ostadix/placement-admission/v2` | `crates/ostadix-api/src/evidence/admit.rs` `PLACEMENT_ADMISSION_DIGEST_DOMAIN_V2` | Current process-portable V6/Graph V2 semantic coordinate consumed by freshly prepared placement V2 authority. |
| Backend catalog | `ostadix.backend-catalog/v5` | `crates/ostadix-api/src/backend_catalog.inc.rs` `current_schema` | Canonical backend specification and implementation-identity projection. V5 extends frozen V4 identity with one explicit optional bounded morphism-profile label per backend. |
| Backend morphism | `ostadix.backend-morphism/v1` | `crates/ostadix-api/src/backend_morphism.rs` `BACKEND_MORPHISM_SCHEMA_V1` | Experimental shadow-only crossing kernel. Catalog V5 binds which profile applies, but does not enforce its assessment through evidence, admission, placement, or dispatch. |
| Hosted transport | `ostadix.hosted-transport/v1`, `ostadix.hosted-transport/v2` | `crates/ostadix-api/src/hosted_remote/protocol.rs` `HOSTED_PROTOCOL_V1`; `crates/ostadix-api/src/hosted_remote/v2/protocol.rs` `HOSTED_PROTOCOL_V2`; `crates/ostadix-api/src/hosted_remote/v2/store.rs` `HOSTED_STATE_AUTHORITY_SCHEMA_V1` = `ostadix.hosted-state-authority/v1` | Frozen single-operation V1 and opt-in durable-session V2 wire contracts. Package 0.3 and later roots bind their durable state to current Graph V2/V6/placement V2 and reject older journals without migration. |
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
| `oexec.evidence/v6` / `ostadix-evidence-bundle/v6` | `EVIDENCE_SCHEMA_V6`; `evidence_bundle_sha256_v6` | Typed per-operation fidelity assessment bound to Graph V2 and the current Catalog V5 projection. |
| `oexec.admission/v6` / `ostadix-execution-admission/v6` | `ADMISSION_SCHEMA_V6`; `admit_execution_v6` | Revalidated local V6 admission accepted by the current coordinator and evaluator. |
| `oexec.admission-why/v2` | `SCHEDULE_WHY_SCHEMA_V2` | Inspection-only V6 why-view retaining typed fidelity evidence. |
| `oexec.schedule-explanation/v2` | `SCHEDULE_EXPLANATION_SCHEMA_V2` | Whole-admission JSON projection whose embedded admission is V6; V1 bytes remain archival. |
| `ostadix/placement-admission/v2` | `PLACEMENT_ADMISSION_DIGEST_DOMAIN_V2` | Process-portable semantic digest over V6 schemas/analyzer, Graph V2 identities, current Catalog V5 projection, and policy. Fresh `PlacementFragmentBindingsV2` binds this digest. |

The unversioned `analyze_execution` and `admit_execution`, `AdmittedExecution`,
uppercase `O`, evaluator/coordinator, CLI JSON, version report, and MCP behavior
now mean V6/Graph V2. Current prepared execution uses
`PlacementFragmentBindingsV2` and `PreparedPlacementFragmentV2`. The public V1
fragment/binding vocabulary and `AdmittedExecutionV5` are inspection-only and
cannot enter current execution or authorization. `ExecutionIntentV1` remains
Graph V1 while binding the current Catalog V5 projection, and execution of a
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
   binds the selected optional profile. The explicit Graph V2/Evidence V6 path
   preserves typed solver assessments, but does not itself enforce morphism
   profiles or silently strengthen or narrow current V6 semantics.
8. At v0.3.0, `ostadix-api` was the narrow stable embedding facade. In the
   post-tag Unreleased tree it is the full runtime engine and owns the advanced
   modules. Root `o_lang::<module>` paths are explicit compatibility reexports
   of those same nominal types, not separately compiled implementations.
9. Package-0.3-and-later hosted state roots carry
   `HOSTED_STATE_AUTHORITY_SCHEMA_V1`. A root containing earlier journals but
   no exact marker is rejected without mutation; there is no journal uplift.

Execution Intent V1 continues to bind the solved graph V1 identity and the
process's current backend-catalog projection. The V5 catalog rollover therefore
changes that catalog binding without changing the graph algorithm or schema.
Archival V4 whole/per-specification helpers remain available for inspection but
never authorize a current Catalog V5-backed placement V2.

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
