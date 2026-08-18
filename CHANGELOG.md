# Changelog

This file records user-facing changes from the point at which changelog
governance was introduced. It does not reconstruct every historical commit.
Package SemVer is only one of the independent coordinates documented in
[docs/VERSIONING.md](docs/VERSIONING.md).

## [Unreleased]

### Changed

- Information Provenance V2 adds an authority-free, content-addressed sidecar
  over frozen Information Atom V1 identities plus a higher-layer execution
  analyzer that admits only its recomputed image. Acquisition origin is
  projected from typed witnesses, enforcement is an orthogonal assurance, and
  contradiction/invalidation remain separate claim-standing records. Recovery
  is contextual and reports intrinsic loss, typed discharges, and unresolved
  obligations; the first execution adapter deliberately reports
  `Unestablished` until producer authentication, procedure resolution, signer
  authorization, receipt currentness, exact plan-node attribution,
  execution/effect fidelity, and morphism fidelity are established.
- The architecture now names predicate, full-image, and projected-image
  admission and states the Codomain Slack Theorem. Current Evidence V5/V6 is
  precisely hard-projection image admission: analyzer-derived legality fields
  are pinned, while historical cost estimates remain explicitly soft.
- `ostadix-api` is now the independent runtime engine rather than a wrapper
  around `o-lang`. It owns the parser, IR, evaluator, HGraph,
  evidence/admission, scheduler, hosted/project/World implementations, runtime
  dependencies, bundled shims, package-local test assets, and AOT source
  bundle. The root `o-lang` crate is a compatibility and CLI shell with a
  one-way exact-version dependency on the engine.
- Historical `o_lang::<module>` imports are explicit reexports of the engine's
  exact nominal types, so callers do not get duplicate evaluator or value
  implementations. Reflected defining paths such as `type_name` may now name
  `ostadix_api`, and direct engine users may use the full advanced module
  surface as well as the concise owned `Runtime` entry point.
- Generated-runtime dependency and source closure now originate from the
  packaged engine rather than workspace-relative root sources. Registry
  publication order is reversed: publish and verify `ostadix-api` first, then
  publish its exact-version `o-lang` shell dependent.

### Release boundary

This post-v0.3 extraction is a next-release source-contract change. Both Rust
packages now use the synchronized `0.4.0` development coordinate; the immutable
`v0.3.0` tag and its historical narrow-facade contract must not be moved or
relabeled.

## [0.3.0] - 2026-08-17

Release notes: [docs/releases/v0.3.0.md](docs/releases/v0.3.0.md).

### Added

- A two-member Cargo workspace retains the root `o-lang` package and adds the
  publishable `ostadix-api` crate: an owned explicit-shim `Runtime`, facade-owned
  parse/evaluate errors, and the complete public `OValue` payload vocabulary.
- Source-release, Dependabot, Docker, CI, and lock-projection contracts now
  distinguish the workspace facade from the independently locked fuzz and MCP
  roots.
- Explicit, source-additive Graph V2, Evidence/Admission V6, placement-admission
  V2 digest, and Why V2 APIs preserve complete typed fidelity assessments.
  Package 0.3 atomically makes those coordinates current for unversioned APIs,
  coordinator/evaluator/CLI/MCP execution, and new placement fragments.
  Execution Intent V1 remains bound to Graph V1 plus Catalog V5, while explicit
  Admission V5, Graph V1, Why V1, and prepared-fragment V1 surfaces are
  archival inspection APIs only: there is no silent V5-to-V6 uplift or hosted
  placement-fragment conversion.
- Durable Hosted V2 state roots carry an exact package-0.3 execution-authority
  marker. Pre-0.3 roots, V1 prepared fragments, and V1 placement-admission
  digests are rejected by current execution rather than migrated or relabeled.
- Experimental Information Bridge V1 adds eight explicit, bounded,
  authority-free native metadata projections plus a lock-free existing-root
  reader, `o-info head`, and fixed local MCP head inspection. HGraph/Evidence
  metadata digests intentionally omit source/value/runtime identity; raw
  registry and Hosted locators/tokens are domain-projected equality oracles,
  not confidentiality primitives.
- A manifest-governed production-library root DAG now closes source geometry,
  compiled fragments, physical overrides, facade projections, allowed edges,
  layer descent, and multi-root cycles under fail-closed CI validation.
- O-core personality smoke failures now capture bounded QMP scheduler, thread,
  frame, and RPC state only after the failure verdict is frozen; successful
  runs remain byte- and behavior-compatible with the prior guest artifacts.

### Changed

- Root and MCP package coordinates advance to `0.3.0`; generated-runtime stays
  frozen at `0.1.0`. Publication, when separately authorized, must publish
  `o-lang` before its exact-version `ostadix-api` dependent.
- Backend catalog V5 now binds the explicit optional BackendMorphism V1 profile
  assignment for all 30 canonical backends. Archival V4 hashes remain frozen;
  the three profiled crossings remain shadow-only and do not change
  `BackendInterface`, HGraph solving, graph hashing, evidence/admission schemas,
  or execution behavior.
- `ParsedDocumentV1.nodes` is private before the 0.3 tag. Use `nodes()` or
  `into_nodes()`; parsed-document equality now also binds parser-captured exact
  source SHA-256 and length. The stable `ostadix-api` facade remains unchanged.
- Canonical placement, backend catalog/state, execution-contract, and graph
  evaluation seams remove wrong-way module dependencies without changing the
  corresponding execution semantics or archived identity formulas.

### Fixed

- O-core x86-64 asynchronous user-frame returns preserve arithmetic status
  flags while clearing hazardous control flags, and the mode-0 proof binds the
  syscall FMASK entry behavior without charging capacity-tight probe modes.
- The hosted durable-state lock explicitly unlocks its final file descriptor
  before drop; a duplicated-descriptor regression proves immediate reopen after
  the final store clone is released.
- Mode-25 crash monitoring preserves the armed lifecycle state across the
  crash transition, and ArtifactId ownership no longer depends on the World
  facade.

### Release boundary

The v0.3.0 tag identifies an advanced systems-research release, not OSTADIX
Alpha qualification or a production-readiness claim. The exact included
claims, explicit nonclaims, independent version coordinates, and source-asset
verification procedure are recorded in
[docs/releases/v0.3.0.md](docs/releases/v0.3.0.md).

## [0.2.0] - 2026-08-16

Release notes: [docs/releases/v0.2.0.md](docs/releases/v0.2.0.md).

### Added

- Contribution, private security-reporting, conduct, and review-ownership
  policies.
- Structured public bug and feature requests plus a pull-request checklist.
- Deterministic governance validation for required repository surfaces and
  version-coordinate documentation.
- Experimental, authority-free Information Kernel V1 records, immutable local
  roots, projection/lift receipts, signed offline delta packs, and the local
  `o info` workflow.
- Experimental BackendMorphism V1 shadow profiles for bounded Python,
  JavaScript, and Rust crossings. These profiles do not change backend catalog
  V4, admission, placement, or dispatch.
- Versioned JSON schedule explanations for machine consumers while retaining
  the existing human-readable explanation by default.
- A non-cloneable Hosted V2 lifetime owner and cloneable request-only handle,
  plus graceful first-signal draining and second-signal forced termination for
  `o-node`.
- Local CI-posture and architectural dependency-boundary validators.

### Changed

- The hosted benchmark consumes typed schedule JSON instead of parsing the
  human explanation line.
- Parser syntax classification, dispatch classification, shared resource
  identities, and placement/catalog compatibility now have lower-level module
  boundaries that remove the first wrong-way dependency edges.
- Generated AOT projects include the canonical CBOR, syntax-dialect,
  dispatch-model, and backend-morphism source closure required by their embedded
  runtime.
- GitHub Actions remain SHA-pinned and now use Node-24-compatible action
  releases; Dependabot covers the fuzz Cargo root and Dockerfile.

### Fixed

- Hosted restart tests now use the deterministic shutdown barrier before
  immediately reopening a durable state root.
- The hosted semantic smoke accepts and verifies the current five-digest V5
  admission binding, including the placement-admission digest.

### Release boundary

The `0.2.0` value recorded for this historical release is a package coordinate.
Its presence here or in archived source does not itself assert a tag,
publication, production readiness, hardware results, remote placement results,
or a live World.
Release entries must be dated and linked to the exact released commit or tag
when a release is cut.

[Unreleased]: https://github.com/lostadi/Ostadix-lang/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/lostadi/Ostadix-lang/tree/v0.3.0
[0.2.0]: https://github.com/lostadi/Ostadix-lang/tree/v0.2.0
