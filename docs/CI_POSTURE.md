# Local CI posture audit

`scripts/local_ci_posture.py` provides a deterministic, local-first audit of
the repository's CI and release posture. The baseline profile uses only the
Python standard library, does not build Ostadix, and is the posture check run by
the required `Contract surfaces` CI lane.

Run the same baseline locally with either human-readable or machine-readable
output:

```bash
python3 scripts/local_ci_posture.py --profile baseline --format text
python3 scripts/local_ci_posture.py --profile baseline --format json
```

The baseline checks:

- immutable full-SHA pins for external Actions, and SHA-256 image digests for
  any `docker://` action;
- high-risk workflow triggers, explicit permissions, and self-hosted or
  dynamically selected runners;
- exact agreement among `ci/required-jobs.toml`, `ci/test-suites.toml`, the
  `required-ci` aggregate, and its stable `Required CI` check name;
- Dependabot coverage for every checked-in Cargo root, Dockerfile directory,
  and GitHub Actions workflow;
- agreement among release metadata, the release-claim/package jobs, the
  contracts lane, and the required source-release surfaces.

## Full local audit

The full profile first runs the baseline, then detects these optional tools on
`PATH`: `actionlint`, `zizmor`, `gitleaks`, `cargo-audit`, `cargo-deny`, and
`git-sizer`.

```bash
python3 scripts/local_ci_posture.py --profile full --format text
```

The script never installs a tool and never launches `cargo` in the live
checkout. It invokes the two Cargo ecosystem analyzers by their direct
`cargo-audit` and `cargo-deny` binaries, with fetching disabled, a temporary
`CARGO_TARGET_DIR`, and locked, offline inputs where supported. Because
`cargo-deny` can internally request Cargo metadata, it runs only against an
isolated temporary mirror that excludes `.git`, build outputs, caches, and
nested checkouts. `cargo-audit` disables advisory fetching and yanked-index
lookups, so it needs an already available local RustSec advisory database.
`cargo-deny` is not run until the repository has an explicitly reviewed
`deny.toml`; the script does not invent license, source, or dependency-ban
policy. Workflow and secret scanners use offline/redacted modes. A before/after
Git status guard detects visible repository mutations.

Missing full-profile tools, Git metadata, the local advisory database, or
required policy configuration make the audit incomplete rather than silently
skipped.

## Optional GitHub inspection

Add `--github` to either profile to inspect remote policy through authenticated
read-only `gh api --method GET` requests:

```bash
python3 scripts/local_ci_posture.py --profile baseline --format json --github
```

This checks that GitHub Actions defaults to read-only permissions without pull
request approval and that the default branch's effective rulesets or legacy
branch protection require pull requests plus the stable `Required CI` status.
Branch names are URL-encoded before inspection. It does not edit repository
settings, rulesets, branch protection, secrets, Actions, Dependabot, or pull
requests. Missing `gh` authentication or permission is reported explicitly.

## Exit and report contract

Both text and JSON contain all checks in deterministic ID/path/line order. JSON
uses schema `ostadix.local-ci-posture/v1`.

- exit `0`: every requested check passed;
- exit `1`: one or more posture findings were found (or an optional GitHub
  inspection was unavailable in the baseline profile);
- exit `2`: the full profile is incomplete because a required optional tool or
  configuration is missing.

This is a static and tool-backed posture audit, not proof that a workflow is
secure or that remote branch policy cannot change after inspection. It never
replaces the independent semantic, Hosted, release, Docker, MCP, or O-core
evidence lanes.
