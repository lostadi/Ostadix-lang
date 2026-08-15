#!/usr/bin/env python3
"""Validate and project Ostadix's duplicated build/CI contract surfaces.

The authoritative data remains in its owning source (Cargo manifests, the
backend catalog, and ci/*.toml).  This tool projects those facts into shell
preflights and rejects copied literals that have drifted.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
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
RUST_TEST_SUPPORT = ROOT / "tests" / "support" / "mod.rs"


class ContractError(RuntimeError):
    pass


def load_toml(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def test_suite_contract() -> tuple[dict[str, dict], dict[str, dict]]:
    document = load_toml(TEST_SUITES)
    if document.get("schema") != "ostadix.ci-test-suites/v2":
        raise ContractError("unsupported CI test-suite schema")
    probes = document.get("runtime_probes")
    if not isinstance(probes, dict) or not probes:
        raise ContractError("CI test-suite manifest has no runtime probes")
    if list(probes) != sorted(probes):
        raise ContractError("runtime probe IDs must be sorted")
    commands: list[str] = []
    for probe_id, probe in probes.items():
        if not re.fullmatch(r"[A-Za-z0-9_.+-]+", probe_id):
            raise ContractError(f"runtime probe {probe_id!r} has an invalid ID")
        if not isinstance(probe, dict) or set(probe) != {"executable", "probe_args"}:
            raise ContractError(
                f"runtime probe {probe_id!r} must contain only executable and probe_args"
            )
        executable = probe["executable"]
        if not isinstance(executable, str) or not re.fullmatch(
            r"[A-Za-z0-9_.+-]+", executable
        ):
            raise ContractError(f"runtime probe {probe_id!r} has an invalid executable")
        probe_args = probe["probe_args"]
        if (
            not isinstance(probe_args, list)
            or not probe_args
            or any(
                not isinstance(argument, str) or not argument or "\0" in argument
                for argument in probe_args
            )
        ):
            raise ContractError(f"runtime probe {probe_id!r} has invalid probe arguments")
        commands.append(executable)
    if len(commands) != len(set(commands)):
        raise ContractError("runtime probe executables must be unique")

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
        unknown = sorted(set(requirements) - set(probes))
        if unknown:
            raise ContractError(
                f"suite {name!r} references unknown runtime probe(s): {', '.join(unknown)}"
            )
    return probes, suites


def runtime_probes() -> dict[str, dict]:
    return test_suite_contract()[0]


def test_suites() -> dict[str, dict]:
    return test_suite_contract()[1]


def suite_runtime_ids(suite_name: str) -> list[str]:
    _, suites = test_suite_contract()
    if suite_name not in suites:
        raise ContractError(f"unknown CI suite {suite_name!r}")
    return suites[suite_name]["required_executables"]


def required_executables(suite_name: str) -> list[str]:
    probes, _ = test_suite_contract()
    return [probes[probe_id]["executable"] for probe_id in suite_runtime_ids(suite_name)]


def probe_runtime(probe_id: str, probe: dict) -> str:
    executable = probe["executable"]
    resolved = shutil.which(executable)
    if resolved is None:
        raise ContractError(
            "runtime-evidence status=missing-required "
            f"policy=required runtime={probe_id} executable={executable}"
        )
    completed = subprocess.run(
        [resolved, *probe["probe_args"]],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if completed.returncode != 0:
        raise ContractError(
            "runtime-evidence status=not-invocable "
            f"policy=required runtime={probe_id} path={resolved} "
            f"exit_code={completed.returncode}"
        )
    output_lines = completed.stdout.splitlines()
    version = output_lines[0] if output_lines else "<no-output>"
    return (
        "runtime-evidence status=invocable "
        f"policy=required runtime={probe_id} path={resolved} version={version}"
    )


def probe_suite(suite_name: str) -> list[str]:
    probes, _ = test_suite_contract()
    return [probe_runtime(probe_id, probes[probe_id]) for probe_id in suite_runtime_ids(suite_name)]


def workflow_jobs(text: str) -> set[str]:
    match = re.search(r"(?m)^jobs:\s*$", text)
    if match is None:
        raise ContractError("CI workflow has no jobs mapping")
    return set(re.findall(r"(?m)^  ([A-Za-z0-9_-]+):\s*$", text[match.end() :]))


def workflow_job_body(text: str, job_name: str) -> str:
    job = re.search(
        rf"(?ms)^  {re.escape(job_name)}:\s*\n(?P<body>.*?)(?=^  [A-Za-z0-9_-]+:\s*$|\Z)",
        text,
    )
    if job is None:
        raise ContractError(f"CI workflow has no {job_name!r} job")
    return job.group("body")


def workflow_job_needs(text: str, job_name: str) -> list[str]:
    """Return one top-level job's block-list `needs` without a YAML dependency."""
    needs = re.search(
        r"(?ms)^    needs:\s*\n(?P<items>(?:^      - [A-Za-z0-9_-]+\s*$\n?)+)",
        workflow_job_body(text, job_name),
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


def validate_runtime_probe_consumers(workflow: str) -> None:
    for suite in ("docker", "rust-hosted", "rust-tests"):
        command = f"python3 scripts/contract_surfaces.py probe-runtimes --suite {suite}"
        if workflow.count(command) != 1:
            raise ContractError(
                f"CI must consume the authoritative runtime probes once for suite {suite!r}"
            )
    support = RUST_TEST_SUPPORT.read_text(encoding="utf-8")
    if 'include_str!("../../ci/test-suites.toml")' not in support:
        raise ContractError(
            "Rust integration-test support does not consume ci/test-suites.toml"
        )

    mcp_job = workflow_job_body(workflow, "mcp")
    component = "components: clippy"
    invocation = "cargo +1.97.1 clippy"
    if component not in mcp_job or invocation not in mcp_job:
        raise ContractError("MCP CI must install and invoke the Clippy component")
    if mcp_job.index(component) > mcp_job.index(invocation):
        raise ContractError("MCP CI must install Clippy before invoking it")


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
    validate_runtime_probe_consumers(workflow)
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
    probe = subparsers.add_parser("probe-runtimes")
    probe.add_argument("--suite", required=True)
    args = parser.parse_args()
    try:
        if args.command == "validate":
            validate()
            print("contract-surfaces: ok")
        elif args.command == "required-executables":
            print("\n".join(required_executables(args.suite)))
        else:
            print("\n".join(probe_suite(args.suite)))
    except (ContractError, OSError, tomllib.TOMLDecodeError) as error:
        print(f"contract-surfaces: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
