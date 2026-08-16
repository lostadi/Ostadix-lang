# Contributing to Ostadix

Ostadix is a research-stage language and systems repository. Contributions are
welcome when they preserve executable capability, make compatibility explicit,
and attach evidence no broader than the substrate actually tested.

## Before opening work

- Search existing issues and keep one change logically focused.
- Report suspected vulnerabilities through the private process in
  [SECURITY.md](SECURITY.md), never in a public issue or pull request.
- Do not submit secrets, credentials, private keys, proprietary fixtures, or
  generated artifacts you do not have the right to contribute.
- Discuss large public-API, protocol, catalog, or file-format changes before
  implementation. These surfaces have independent version axes described in
  [docs/VERSIONING.md](docs/VERSIONING.md).

## Local-first development

Use a local clone and the smallest relevant gate. The baseline governance and
contract checks require only Python's standard library:

```sh
python3 scripts/local_ci_posture.py --profile baseline
python3 scripts/contract_surfaces.py validate
python3 -m unittest -v tests.test_governance_surfaces
```

For O-language behavior, use the checked-in backends explicitly:

```sh
O examples/hello.O backends
```

That smoke currently reports `[number] 2`. It proves only that example on the
local hosted substrate. It is not evidence of every backend, native O-core
hardware, remote placement, or a live World.

Rust changes should run the repository's relevant format, lint, and test gates
with the pinned toolchain. Run broad suites only when their prerequisites are
available; record skipped tools and environments instead of treating them as
passes. The CI suite map in `ci/test-suites.toml` is the machine-readable test
contract.

## Change expectations

- Preserve existing execution backends and ordinary capacity unless the change
  explicitly deprecates a surface and documents the migration.
- Keep wire, evidence, admission, catalog, placement, World, and information
  schema changes explicit. Do not silently relabel or uplift old records.
- Add positive and negative tests at the boundary being changed.
- Use typed errors at public boundaries; avoid exposing incidental internal
  error strings as a compatibility contract.
- Update user-facing documentation and the `[Unreleased]` changelog entry when
  behavior or a supported contract changes.
- Keep generated/AOT source closure synchronized when runtime code embedded by
  generated projects changes.

## Pull requests

Complete the pull-request template with:

1. the problem and resulting behavior;
2. compatibility and execution-capacity impact;
3. exact commands run and their outcomes; and
4. explicit environments, backends, or claims not tested.

Small commits are appreciated, but no particular commit shape is required.
Only submit work you have the right to license under the repository's
`LGPL-2.1-only` license. Maintainers may ask for a narrower patch or stronger
evidence before merge.
