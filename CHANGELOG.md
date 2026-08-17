# Changelog

This file records user-facing changes from the point at which changelog
governance was introduced. It does not reconstruct every historical commit.
Package SemVer is only one of the independent coordinates documented in
[docs/VERSIONING.md](docs/VERSIONING.md).

## [Unreleased]

### Added

- Explicit, source-additive Graph V2, Evidence/Admission V6, placement-admission
  V2 digest, and Why V2 APIs preserve complete typed fidelity assessments.
  Current unversioned APIs, coordinator/evaluator/CLI/MCP behavior, and
  Execution Intent V1 remain V5/Graph V1; there is no silent V5-to-V6 uplift or
  hosted placement-fragment conversion.

### Changed

- Backend catalog V5 now binds the explicit optional BackendMorphism V1 profile
  assignment for all 30 canonical backends. Archival V4 hashes remain frozen;
  the three profiled crossings remain shadow-only and do not change
  `BackendInterface`, HGraph solving, graph hashing, evidence/admission schemas,
  or execution behavior.

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

The current `0.2.0` value in `Cargo.toml` is a package coordinate. Its presence
here or in source does not itself assert a tag, publication, production
readiness, hardware result, remote placement result, or live-World result.
Release entries must be dated and linked to the exact released commit or tag
when a release is cut.

[Unreleased]: https://github.com/lostadi/Ostadix-lang/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/lostadi/Ostadix-lang/releases/tag/v0.2.0
