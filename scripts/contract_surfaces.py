#!/usr/bin/env python3
"""Validate and project Ostadix's duplicated build/CI contract surfaces.

The authoritative data remains in its owning source (Cargo manifests, the
backend catalog, and ci/*.toml).  This tool projects those facts into shell
preflights and rejects copied literals that have drifted.
"""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
TEST_SUITES = ROOT / "ci" / "test-suites.toml"
REQUIRED_JOBS = ROOT / "ci" / "required-jobs.toml"
CI_WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
FUZZ_WORKFLOW = ROOT / ".github" / "workflows" / "fuzz.yml"
CATALOG = ROOT / "src" / "backend_catalog.inc.rs"
MCP_SMOKE = ROOT / "scripts" / "smoke_ostadix_mcp.py"


class ContractError(RuntimeError):
    pass


def load_toml(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def test_suites() -> dict[str, dict]:
    document = load_toml(TEST_SUITES)
    if document.get("schema") != "ostadix.ci-test-suites/v1":
        raise ContractError("unsupported CI test-suite schema")
    suites = document.get("suites")
    if not isinstance(suites, dict) or not suites:
        raise ContractError("CI test-suite manifest has no suites")
    for name, suite in suites.items():
        requirements = suite.get("required_executables")
        if not isinstance(requirements, list) or not requirements:
            raise ContractError(f"suite {name!r} has no required executables")
        if requirements != sorted(set(requirements)):
            raise ContractError(
                f"suite {name!r} required executables must be sorted and unique"
            )
        if any(not re.fullmatch(r"[A-Za-z0-9_.+-]+", item) for item in requirements):
            raise ContractError(f"suite {name!r} contains an invalid executable name")
    return suites


def workflow_jobs(text: str) -> set[str]:
    match = re.search(r"(?m)^jobs:\s*$", text)
    if match is None:
        raise ContractError("CI workflow has no jobs mapping")
    return set(re.findall(r"(?m)^  ([A-Za-z0-9_-]+):\s*$", text[match.end() :]))


def workflow_job_needs(text: str, job_name: str) -> list[str]:
    """Return one top-level job's block-list `needs` without a YAML dependency."""
    job = re.search(
        rf"(?ms)^  {re.escape(job_name)}:\s*\n(?P<body>.*?)(?=^  [A-Za-z0-9_-]+:\s*$|\Z)",
        text,
    )
    if job is None:
        raise ContractError(f"CI workflow has no {job_name!r} job")
    needs = re.search(
        r"(?ms)^    needs:\s*\n(?P<items>(?:^      - [A-Za-z0-9_-]+\s*$\n?)+)",
        job.group("body"),
    )
    if needs is None:
        raise ContractError(f"CI job {job_name!r} has no block-list needs")
    return re.findall(r"(?m)^      - ([A-Za-z0-9_-]+)\s*$", needs.group("items"))


def catalog_schema() -> str:
    match = re.search(
        r'backend_catalog_metadata!\s*\{\s*current_schema:\s*"([^"]+)"',
        CATALOG.read_text(encoding="utf-8"),
        re.DOTALL,
    )
    if match is None:
        raise ContractError("backend catalog does not declare a schema")
    return match.group(1)


def validate_manifest_versions() -> None:
    for path in (ROOT / "Cargo.toml", ROOT / "mcp/ostadix_lang_mcp_server/Cargo.toml"):
        package = load_toml(path).get("package", {})
        if package.get("rust-version") != "1.93.1":
            raise ContractError(f"{path.relative_to(ROOT)} must declare rust-version 1.93.1")
    toolchain = load_toml(ROOT / "rust-toolchain.toml").get("toolchain", {})
    if toolchain.get("channel") != "1.97.1":
        raise ContractError("rust-toolchain.toml must pin release Rust 1.97.1")


def validate_action_pins() -> None:
    for path in (CI_WORKFLOW, FUZZ_WORKFLOW):
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            match = re.search(r"\buses:\s*([^\s#]+)", line)
            if match is None or match.group(1).startswith("./"):
                continue
            action = match.group(1)
            if not re.fullmatch(r"[^@]+@[0-9a-f]{40}", action):
                raise ContractError(
                    f"{path.relative_to(ROOT)}:{line_number} action is not pinned to a full SHA: {action}"
                )


def validate() -> None:
    suites = test_suites()
    required = load_toml(REQUIRED_JOBS)
    if required.get("schema") != "ostadix.ci-required-jobs/v1":
        raise ContractError("unsupported required-jobs schema")
    required_jobs = required.get("required_jobs")
    if not isinstance(required_jobs, list) or required_jobs != sorted(set(required_jobs)):
        raise ContractError("required_jobs must be a sorted unique list")
    workflow = CI_WORKFLOW.read_text(encoding="utf-8")
    jobs = workflow_jobs(workflow)
    missing_jobs = sorted(set(required_jobs) - jobs)
    if missing_jobs:
        raise ContractError(f"CI workflow is missing required job(s): {', '.join(missing_jobs)}")
    missing_suites = sorted(set(required_jobs) - set(suites))
    if missing_suites:
        raise ContractError(
            f"test-suite manifest is missing required job(s): {', '.join(missing_suites)}"
        )
    aggregate_needs = workflow_job_needs(workflow, "required-ci")
    if aggregate_needs != sorted(set(aggregate_needs)):
        raise ContractError("required-ci needs must be sorted and unique")
    if set(aggregate_needs) != set(required_jobs):
        missing = sorted(set(required_jobs) - set(aggregate_needs))
        extra = sorted(set(aggregate_needs) - set(required_jobs))
        details = []
        if missing:
            details.append(f"missing: {', '.join(missing)}")
        if extra:
            details.append(f"extra: {', '.join(extra)}")
        raise ContractError(
            "required-ci needs do not match required_jobs (" + "; ".join(details) + ")"
        )
    validate_manifest_versions()
    validate_action_pins()
    catalog_schema()
    smoke = MCP_SMOKE.read_text(encoding="utf-8")
    if (
        "_current_catalog_schema(root)" not in smoke
        or 'f"runtime-catalog-schema={catalog_schema}"' not in smoke
    ):
        raise ContractError(
            "MCP smoke does not derive the backend catalog schema from the canonical catalog"
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("validate")
    required = subparsers.add_parser("required-executables")
    required.add_argument("--suite", required=True)
    args = parser.parse_args()
    try:
        if args.command == "validate":
            validate()
            print("contract-surfaces: ok")
        else:
            suites = test_suites()
            if args.suite not in suites:
                raise ContractError(f"unknown CI suite {args.suite!r}")
            print("\n".join(suites[args.suite]["required_executables"]))
    except (ContractError, OSError, tomllib.TOMLDecodeError) as error:
        print(f"contract-surfaces: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
