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
| Rust package | `0.2.0` | `Cargo.toml` `package.version` | Source/library/CLI release identity. SemVer applies only to the documented public façade. |
| Minimum Rust | `1.93.1` | `Cargo.toml` `package.rust-version` | Lowest compiler supported by the MSRV contract. This is not the package version or release compiler. |
| Release toolchain | `1.97.1` | `rust-toolchain.toml` `toolchain.channel` | Pinned compiler for release, formatting, Clippy, and generated-runtime evidence. This may advance without changing the MSRV. |
| Execution intent | `oexec.execution-intent/v1` | `src/evidence/intent.rs` `EXECUTION_INTENT_SCHEMA_V1` | Stable, authority-free identity of exact source and analyzed semantics. |
| Evidence | `oexec.evidence/v5` | `src/evidence/fact.rs` `EVIDENCE_SCHEMA_V5` | Pre-execution evidence vocabulary; evidence is not admission. |
| Admission | `oexec.admission/v5` | `src/evidence/fact.rs` `ADMISSION_SCHEMA_V5` | Live process-local admitted-execution contract. |
| Backend catalog | `ostadix.backend-catalog/v4` | `src/backend_catalog.inc.rs` `current_schema` | Canonical backend specification and implementation-identity projection. |
| Backend morphism | `ostadix.backend-morphism/v1` | `src/backend_morphism.rs` `BACKEND_MORPHISM_SCHEMA_V1` | Experimental shadow-only crossing profiles; this coordinate is not part of catalog V4, evidence, admission, placement, or dispatch. |
| Hosted transport | `ostadix.hosted-transport/v1`, `ostadix.hosted-transport/v2` | `src/hosted_remote/protocol.rs` `HOSTED_PROTOCOL_V1`; `src/hosted_remote/v2/protocol.rs` `HOSTED_PROTOCOL_V2` | Frozen single-operation V1 and opt-in durable-session V2 wire contracts. |
| Placement milestone | Hosted Placement V6 | `docs/HOSTED_PLACEMENT_V6.md` | A placement/evidence milestone name, not a source-level schema constant and not an automatic upgrade from Admission V5. |
| World wire family | V1 | `src/world/protocol.rs` `WORLD_SCHEMA_V1`; codec constants under `src/world/` | Offline World record, identity, value, and receipt codecs. These coordinates do not establish a live World. |
| Information kernel family | V1 | `INFORMATION_SCHEMA_V1` and record constants under `src/information/` | Authority-free, immutable information identities, deltas, projections, and receipts. Each record validates its own exact schema. |

“V6” in Hosted Placement V6 names the placement milestone and evidence model;
it is not a promise that every schema or transport is numerically version 6.

### World V1 coordinates

The World row is a family, not one interchangeable integer. Its current source
constants are `WORLD_SCHEMA_V1` in `src/world/protocol.rs`,
`WORLD_WIRE_CODEC_VERSION` in `src/world/codec.rs`, `IDENTITY_WIRE_VERSION` in
`src/world/identity_wire.rs`, `OVALUE_WIRE_SCHEMA_V1` in
`src/world/value_codec.rs`, and `WORLD_RECEIPT_SCHEMA_V1` in
`src/world/receipt_codec.rs`. All currently carry coordinate `1`, but a future
change to one does not silently change the others.

### Information-kernel V1 coordinates

`INFORMATION_SCHEMA_V1` in `src/information/mod.rs` names the family. Individual
records retain their own coordinates:

| Record | Coordinate | Source constant |
|---|---|---|
| Family | `ostadix.information/v1` | `src/information/mod.rs` `INFORMATION_SCHEMA_V1` |
| Atom | `ostadix.info-atom/v1` | `src/information/model.rs` `INFORMATION_ATOM_SCHEMA_V1` |
| Entity | `ostadix.info-entity/v1` | `src/information/model.rs` `ENTITY_DESCRIPTOR_SCHEMA_V1` |
| Snapshot | `ostadix.info-snapshot/v1` | `src/information/root.rs` `INFORMATION_SNAPSHOT_SCHEMA_V1` |
| Revision | `ostadix.info-revision/v1` | `src/information/root.rs` `INFORMATION_REVISION_SCHEMA_V1` |
| Delta | `ostadix.info-delta/v1` | `src/information/delta.rs` `INFORMATION_DELTA_SCHEMA_V1` |
| Projection receipt | `ostadix.projection-receipt/v1` | `src/information/projection.rs` `PROJECTION_RECEIPT_SCHEMA_V1` |
| Offline delta pack | `ostadix.info-delta-pack/v1` | `src/information/exchange.rs` `INFORMATION_DELTA_PACK_SCHEMA_V1` |
| Signed offline delta pack | `ostadix.signed-info-delta-pack/v1` | `src/information/exchange.rs` `SIGNED_INFORMATION_DELTA_PACK_SCHEMA_V1` |
| Decision receipt | `ostadix.info-decision/v1` | `src/information/decision.rs` `DECISION_RECEIPT_SCHEMA_V1` |
| Observation | `ostadix.info-observation/v1` | `src/information/decision.rs` `OBSERVATION_RECORD_SCHEMA_V1` |

`LossContractV1` is a typed component of the projection receipt rather than an
independently labeled wire schema. Changing its serialized meaning therefore
requires advancing the containing receipt coordinate.

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
7. Backend-morphism V1 is an inspection-only profile family. Enforcing it
   requires an explicit backend-catalog/evidence version rollover; it cannot
   silently strengthen or narrow catalog V4.
8. The `o_lang::api` façade is the intended embedding surface. Historical
   top-level modules remain available during the 0.2 compatibility period but
   are not all promised as stable external contracts.

## Changing a coordinate

Any version change must update its canonical constant, compatibility tests,
machine-readable version report where that axis is represented, release
documentation, generated/AOT source closure where applicable, and
mutation/rejection tests for the prior identity. Package, MSRV, toolchain, and
citation metadata are validated separately so a toolchain upgrade does not
masquerade as a package or protocol release. A new information record schema
must also state its canonical identity inputs, loss or invalidation behavior,
and whether older records remain inspectable, projectable, or admissible.
