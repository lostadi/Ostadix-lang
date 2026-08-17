#!/usr/bin/env python3
"""Audit the local Ostadix CI posture without changing repository or GitHub state.

The baseline profile is intentionally Python-standard-library only.  The full
profile discovers optional external analyzers and invokes their read-only
interfaces directly.  It never installs a tool, and any ``cargo`` process
started internally by cargo-deny is confined to a temporary repository mirror.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import tempfile
import tomllib
from pathlib import Path
from typing import Callable, Sequence
from urllib.parse import quote


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_DIRECTORY = Path(".github/workflows")
CI_WORKFLOW = Path(".github/workflows/ci.yml")
DEPENDABOT = Path(".github/dependabot.yml")
REQUIRED_JOBS = Path("ci/required-jobs.toml")
TEST_SUITES = Path("ci/test-suites.toml")
SOURCE_RELEASE = Path("scripts/build_source_release.py")

REPORT_SCHEMA = "ostadix.local-ci-posture/v1"
FULL_TOOLS = (
    "actionlint",
    "zizmor",
    "gitleaks",
    "cargo-audit",
    "cargo-deny",
    "git-sizer",
)
RISKY_TRIGGERS = frozenset(
    {
        "issue_comment",
        "issues",
        "pull_request_target",
        "repository_dispatch",
        "workflow_run",
    }
)
RELEASE_SURFACE_PATHS = (
    "docs/CI_POSTURE.md",
    "scripts/local_ci_posture.py",
    "tests/test_local_ci_posture.py",
)
EXCLUDED_WALK_DIRECTORIES = frozenset(
    {
        ".git",
        ".hypothesis",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
        ".tox",
        ".venv",
        "__pycache__",
        "build",
        "dist",
        "target",
    }
)

Runner = Callable[..., subprocess.CompletedProcess[str]]
Which = Callable[[str], str | None]


def _default_runner(command: Sequence[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, **kwargs)  # type: ignore[arg-type]


class PostureReport:
    def __init__(self, profile: str, github_requested: bool) -> None:
        self.profile = profile
        self.github_requested = github_requested
        self.checks: list[dict[str, object]] = []

    def add(
        self,
        status: str,
        check_id: str,
        message: str,
        *,
        path: str | None = None,
        line: int | None = None,
    ) -> None:
        record: dict[str, object] = {
            "id": check_id,
            "status": status,
            "message": message,
        }
        if path is not None:
            record["path"] = path
        if line is not None:
            record["line"] = line
        self.checks.append(record)

    def passed(self, check_id: str, message: str) -> None:
        self.add("pass", check_id, message)

    def finding(
        self,
        check_id: str,
        message: str,
        *,
        path: str | None = None,
        line: int | None = None,
    ) -> None:
        self.add("finding", check_id, message, path=path, line=line)

    def missing(self, check_id: str, message: str) -> None:
        self.add("missing", check_id, message)

    def ordered_checks(self) -> list[dict[str, object]]:
        return sorted(
            self.checks,
            key=lambda record: (
                str(record["id"]),
                str(record.get("path", "")),
                int(record.get("line", 0)),
                str(record["message"]),
            ),
        )

    def summary(self) -> dict[str, int]:
        checks = self.ordered_checks()
        return {
            "pass": sum(record["status"] == "pass" for record in checks),
            "findings": sum(record["status"] == "finding" for record in checks),
            "missing": sum(record["status"] == "missing" for record in checks),
        }

    def exit_code(self) -> int:
        summary = self.summary()
        if self.profile == "full" and summary["missing"]:
            return 2
        if summary["findings"] or summary["missing"]:
            return 1
        return 0

    def document(self) -> dict[str, object]:
        code = self.exit_code()
        status = "pass" if code == 0 else "findings" if code == 1 else "incomplete"
        return {
            "schema": REPORT_SCHEMA,
            "profile": self.profile,
            "github_requested": self.github_requested,
            "status": status,
            "exit_code": code,
            "summary": self.summary(),
            "checks": self.ordered_checks(),
        }


def _relative(root: Path, path: Path) -> str:
    try:
        return path.relative_to(root).as_posix()
    except ValueError:
        return path.as_posix()


def _read_text(report: PostureReport, root: Path, path: Path, check_id: str) -> str | None:
    absolute = root / path
    try:
        return absolute.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        report.finding(check_id, f"cannot read required file: {error}", path=path.as_posix())
        return None


def _load_toml(
    report: PostureReport, root: Path, path: Path, check_id: str
) -> dict[str, object] | None:
    absolute = root / path
    try:
        with absolute.open("rb") as handle:
            return tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as error:
        report.finding(check_id, f"cannot load required TOML: {error}", path=path.as_posix())
        return None


def _strip_yaml_scalar(value: str) -> str:
    value = value.strip()
    if not value:
        return value
    if value[0] in {'"', "'"} and len(value) >= 2 and value[-1] == value[0]:
        return value[1:-1]
    return value.split(" #", 1)[0].strip()


def _workflow_paths(root: Path) -> list[Path]:
    directory = root / WORKFLOW_DIRECTORY
    if not directory.is_dir():
        return []
    return sorted(
        (
            path
            for path in directory.iterdir()
            if path.is_file() and path.suffix in {".yml", ".yaml"}
        ),
        key=lambda path: path.name,
    )


def audit_action_pins(report: PostureReport, root: Path, workflows: Sequence[Path]) -> None:
    findings = 0
    uses_pattern = re.compile(r"^\s*(?:-\s+)?uses:\s*(?P<value>.+?)\s*$")
    repository_action = re.compile(r"[^\s@]+@[0-9a-f]{40}")
    container_action = re.compile(r"docker://[^\s@]+@sha256:[0-9a-f]{64}")
    for path in workflows:
        relative = _relative(root, path)
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            match = uses_pattern.match(line)
            if match is None:
                continue
            action = _strip_yaml_scalar(match.group("value"))
            if action.startswith("./"):
                continue
            valid = (
                container_action.fullmatch(action) is not None
                if action.startswith("docker://")
                else repository_action.fullmatch(action) is not None
            )
            if not valid:
                findings += 1
                report.finding(
                    "baseline.actions.full-sha",
                    f"external action is not pinned to an immutable full digest: {action}",
                    path=relative,
                    line=line_number,
                )
    if findings == 0:
        report.passed(
            "baseline.actions.full-sha",
            f"all external actions in {len(workflows)} workflow(s) use full immutable digests",
        )


def _workflow_triggers(text: str) -> list[tuple[str, int]]:
    lines = text.splitlines()
    for index, line in enumerate(lines):
        match = re.match(r"^on:\s*(?P<inline>.*?)\s*$", line)
        if match is None:
            continue
        inline = _strip_yaml_scalar(match.group("inline"))
        if inline:
            if inline.startswith("[") and inline.endswith("]"):
                return [
                    (token.strip().strip("'\""), index + 1)
                    for token in inline[1:-1].split(",")
                    if token.strip()
                ]
            if inline.startswith("{") and inline.endswith("}"):
                return [
                    (key.group(1), index + 1)
                    for key in re.finditer(
                        r"(?:^|[,{\s])([A-Za-z0-9_-]+)\s*:", inline
                    )
                ]
            return [(inline, index + 1)]
        triggers: list[tuple[str, int]] = []
        for child_index in range(index + 1, len(lines)):
            child = lines[child_index]
            if child.strip() and not child.lstrip().startswith("#"):
                indent = len(child) - len(child.lstrip(" "))
                if indent == 0:
                    break
                key = re.match(r"^  ([A-Za-z0-9_-]+):", child)
                if key is not None:
                    triggers.append((key.group(1), child_index + 1))
        return triggers
    return []


def _permission_blocks(text: str) -> list[tuple[int, int, str, list[tuple[str, str, int]]]]:
    lines = text.splitlines()
    blocks: list[tuple[int, int, str, list[tuple[str, str, int]]]] = []
    for index, line in enumerate(lines):
        match = re.match(r"^(?P<indent>\s*)permissions:\s*(?P<inline>.*?)\s*$", line)
        if match is None:
            continue
        indent = len(match.group("indent"))
        inline = _strip_yaml_scalar(match.group("inline"))
        entries: list[tuple[str, str, int]] = []
        if inline:
            entries.append(("*", inline, index + 1))
        else:
            for child_index in range(index + 1, len(lines)):
                child = lines[child_index]
                if not child.strip() or child.lstrip().startswith("#"):
                    continue
                child_indent = len(child) - len(child.lstrip(" "))
                if child_indent <= indent:
                    break
                entry = re.match(
                    r"^\s+(?P<scope>[A-Za-z0-9_-]+):\s*(?P<access>[^#]+?)\s*$",
                    child,
                )
                if entry is not None:
                    entries.append(
                        (
                            entry.group("scope"),
                            _strip_yaml_scalar(entry.group("access")),
                            child_index + 1,
                        )
                    )
        blocks.append((indent, index + 1, inline, entries))
    return blocks


def _permission_requires_review(scope: str, access: str) -> bool:
    if "${{" in access or access == "write-all":
        return True
    if scope != "*":
        return access == "write"
    # Inline mappings are kept as one scalar by the deliberately small YAML
    # reader.  Still recognize write grants in forms such as
    # ``permissions: {contents: write, actions: read}``.
    return re.search(
        r"(?:^|[,{\s])[A-Za-z0-9_-]+\s*:\s*write(?:$|[,}\s])",
        access,
    ) is not None


def audit_workflow_safety(report: PostureReport, root: Path, workflows: Sequence[Path]) -> None:
    trigger_findings = 0
    permission_findings = 0
    runner_findings = 0
    for path in workflows:
        relative = _relative(root, path)
        text = path.read_text(encoding="utf-8")
        triggers = _workflow_triggers(text)
        if not triggers:
            trigger_findings += 1
            report.finding(
                "baseline.workflows.risky-triggers",
                "workflow has no statically inspectable top-level trigger",
                path=relative,
            )
        for trigger, line_number in triggers:
            if trigger in RISKY_TRIGGERS:
                trigger_findings += 1
                report.finding(
                    "baseline.workflows.risky-triggers",
                    f"high-risk trigger requires explicit review: {trigger}",
                    path=relative,
                    line=line_number,
                )

        blocks = _permission_blocks(text)
        if not any(indent == 0 for indent, _, _, _ in blocks):
            permission_findings += 1
            report.finding(
                "baseline.workflows.permissions",
                "workflow does not declare top-level permissions",
                path=relative,
            )
        for _, line_number, inline, entries in blocks:
            if inline and _permission_requires_review("*", inline):
                permission_findings += 1
                report.finding(
                    "baseline.workflows.permissions",
                    f"permissions are broad or dynamic: {inline}",
                    path=relative,
                    line=line_number,
                )
                continue
            for scope, access, entry_line in entries:
                if _permission_requires_review(scope, access):
                    permission_findings += 1
                    report.finding(
                        "baseline.workflows.permissions",
                        f"permission requires review: {scope}: {access}",
                        path=relative,
                        line=entry_line,
                    )

        lines = text.splitlines()
        for index, line in enumerate(lines):
            match = re.match(r"^\s*runs-on:\s*(?P<value>.*?)\s*$", line)
            if match is None:
                continue
            value = _strip_yaml_scalar(match.group("value"))
            candidate_lines = [(value, index + 1)]
            if not value:
                base_indent = len(line) - len(line.lstrip(" "))
                for child_index in range(index + 1, len(lines)):
                    child = lines[child_index]
                    if child.strip() and len(child) - len(child.lstrip(" ")) <= base_indent:
                        break
                    candidate_lines.append((child.strip(), child_index + 1))
            for candidate, candidate_line in candidate_lines:
                if "self-hosted" in candidate or "${{" in candidate:
                    runner_findings += 1
                    report.finding(
                        "baseline.workflows.self-hosted",
                        f"self-hosted or dynamic runner selection requires review: {candidate}",
                        path=relative,
                        line=candidate_line,
                    )

    if trigger_findings == 0:
        report.passed(
            "baseline.workflows.risky-triggers",
            "no high-risk workflow trigger is enabled",
        )
    if permission_findings == 0:
        report.passed(
            "baseline.workflows.permissions",
            "all workflows declare read-only or empty explicit permissions",
        )
    if runner_findings == 0:
        report.passed(
            "baseline.workflows.self-hosted",
            "no workflow selects a self-hosted or dynamic runner",
        )


def _workflow_jobs(text: str) -> set[str]:
    jobs = re.search(r"(?m)^jobs:\s*$", text)
    if jobs is None:
        return set()
    return set(re.findall(r"(?m)^  ([A-Za-z0-9_-]+):\s*$", text[jobs.end() :]))


def _workflow_job_body(text: str, job_name: str) -> str | None:
    match = re.search(
        rf"(?ms)^  {re.escape(job_name)}:\s*\n(?P<body>.*?)(?=^  [A-Za-z0-9_-]+:\s*$|\Z)",
        text,
    )
    return None if match is None else match.group("body")


def _workflow_job_needs(body: str) -> list[str]:
    inline = re.search(r"(?m)^    needs:\s*([A-Za-z0-9_-]+)\s*$", body)
    if inline is not None:
        return [inline.group(1)]
    block = re.search(
        r"(?ms)^    needs:\s*\n(?P<items>(?:^      - [A-Za-z0-9_-]+\s*$\n?)+)",
        body,
    )
    if block is None:
        return []
    return re.findall(r"(?m)^      - ([A-Za-z0-9_-]+)\s*$", block.group("items"))


def audit_required_wiring(report: PostureReport, root: Path) -> None:
    required_document = _load_toml(
        report, root, REQUIRED_JOBS, "baseline.required-aggregate"
    )
    suite_document = _load_toml(report, root, TEST_SUITES, "baseline.required-aggregate")
    workflow = _read_text(report, root, CI_WORKFLOW, "baseline.required-aggregate")
    if required_document is None or suite_document is None or workflow is None:
        return

    problems: list[str] = []
    if required_document.get("schema") != "ostadix.ci-required-jobs/v1":
        problems.append("required-jobs manifest has an unsupported schema")
    raw_jobs = required_document.get("required_jobs")
    required_jobs = raw_jobs if isinstance(raw_jobs, list) else []
    if not required_jobs or any(not isinstance(job, str) for job in required_jobs):
        problems.append("required_jobs must be a nonempty string list")
        required_jobs = []
    elif required_jobs != sorted(set(required_jobs)):
        problems.append("required_jobs must be sorted and unique")

    suites = suite_document.get("suites")
    suite_names = set(suites) if isinstance(suites, dict) else set()
    missing_suites = sorted(set(required_jobs) - suite_names)
    if missing_suites:
        problems.append("test-suite manifest omits: " + ", ".join(missing_suites))
    unexpected_suites = sorted(suite_names - set(required_jobs))
    if unexpected_suites:
        problems.append(
            "test-suite manifest has non-required suites: "
            + ", ".join(unexpected_suites)
        )

    workflow_jobs = _workflow_jobs(workflow)
    missing_workflow_jobs = sorted(set(required_jobs) - workflow_jobs)
    if missing_workflow_jobs:
        problems.append("CI workflow omits: " + ", ".join(missing_workflow_jobs))

    aggregate = _workflow_job_body(workflow, "required-ci")
    if aggregate is None:
        problems.append("CI workflow has no required-ci aggregate")
    else:
        needs = _workflow_job_needs(aggregate)
        if needs != sorted(set(needs)) or set(needs) != set(required_jobs):
            problems.append("required-ci needs do not exactly match required_jobs")
        if "name: Required CI" not in aggregate:
            problems.append("required-ci does not retain the stable Required CI check name")
        if "if: ${{ always() }}" not in aggregate:
            problems.append("required-ci is not guarded by always()")
        for job in required_jobs:
            if f"${{{{ needs.{job}.result }}}}" not in aggregate:
                problems.append(f"required-ci does not inspect {job!r} result")
        if '[[ "$result" == success ]]' not in aggregate:
            problems.append("required-ci does not fail non-success dependency results")

    if problems:
        for problem in problems:
            report.finding(
                "baseline.required-aggregate",
                problem,
                path=CI_WORKFLOW.as_posix(),
            )
    else:
        report.passed(
            "baseline.required-aggregate",
            f"required-ci exactly aggregates {len(required_jobs)} manifest-owned jobs",
        )


def _dependabot_updates(text: str) -> list[tuple[str, str]]:
    updates: list[tuple[str, str]] = []
    ecosystem: str | None = None
    directory: str | None = None
    for line in text.splitlines():
        start = re.match(r"^\s*-\s+package-ecosystem:\s*(.+?)\s*$", line)
        if start is not None:
            if ecosystem is not None and directory is not None:
                updates.append((ecosystem, directory))
            ecosystem = _strip_yaml_scalar(start.group(1))
            directory = None
            continue
        if ecosystem is not None:
            match = re.match(r"^\s+directory:\s*(.+?)\s*$", line)
            if match is not None:
                directory = _strip_yaml_scalar(match.group(1))
    if ecosystem is not None and directory is not None:
        updates.append((ecosystem, directory))
    return updates


def _manifest_directories(root: Path) -> set[tuple[str, str]]:
    expected: set[tuple[str, str]] = set()
    cargo_manifests = set(_repository_files(root, "Cargo.toml"))
    workspace_manifests = _root_workspace_member_manifests(root)
    for current, directories, files in os.walk(root):
        current_path = Path(current)
        # A developer may keep an ignored comparison checkout below the real
        # root.  Its manifests are not dependency surfaces of this repository.
        if current_path != root and (current_path / ".git").exists():
            directories[:] = []
            continue
        directories[:] = sorted(
            directory
            for directory in directories
            if directory not in EXCLUDED_WALK_DIRECTORIES
        )
        relative = current_path.relative_to(root)
        dependabot_directory = "/" if relative == Path(".") else f"/{relative.as_posix()}"
        if "Cargo.toml" in files:
            manifest = current_path / "Cargo.toml"
            if workspace_manifests is None or manifest not in workspace_manifests:
                expected.add(("cargo", dependabot_directory))
        if "Dockerfile" in files:
            expected.add(("docker", dependabot_directory))
    if _workflow_paths(root):
        expected.add(("github-actions", "/"))
    if workspace_manifests and cargo_manifests & workspace_manifests:
        expected.add(("cargo", "/"))
    return expected


def _root_workspace_member_manifests(root: Path) -> set[Path] | None:
    """Return manifests covered by the root Cargo workspace.

    ``None`` is deliberately fail-closed: malformed workspace declarations make
    every discovered manifest require its own Dependabot entry rather than
    silently treating an unproved member as covered by ``/``.
    """

    root_manifest = root / "Cargo.toml"
    if not root_manifest.is_file():
        return set()
    try:
        cargo = tomllib.loads(root_manifest.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError):
        return None
    workspace = cargo.get("workspace")
    if workspace is None:
        return {root_manifest}
    if not isinstance(workspace, dict):
        return None
    members = workspace.get("members")
    excludes = workspace.get("exclude", [])
    if (
        not isinstance(members, list)
        or not members
        or any(not isinstance(item, str) or not item for item in members)
        or not isinstance(excludes, list)
        or any(not isinstance(item, str) or not item for item in excludes)
    ):
        return None

    def expand(patterns: list[str]) -> set[Path] | None:
        manifests: set[Path] = set()
        for pattern in patterns:
            pure = Path(pattern)
            if pure.is_absolute() or ".." in pure.parts:
                return None
            matches = [root] if pattern == "." else sorted(root.glob(pattern))
            if not matches:
                return None
            for match in matches:
                manifest = match if match.name == "Cargo.toml" else match / "Cargo.toml"
                if manifest.is_file():
                    manifests.add(manifest)
        return manifests

    covered = expand(members)
    excluded = expand(excludes) if excludes else set()
    if covered is None or excluded is None:
        return None
    covered.add(root_manifest)
    return covered - excluded


def _independent_cargo_manifests(root: Path) -> list[Path]:
    manifests = _repository_files(root, "Cargo.toml")
    covered = _root_workspace_member_manifests(root)
    if covered is None:
        return manifests
    return [
        manifest
        for manifest in manifests
        if manifest == root / "Cargo.toml" or manifest not in covered
    ]


def _repository_files(root: Path, basename: str) -> list[Path]:
    matches: list[Path] = []
    for current, directories, files in os.walk(root):
        current_path = Path(current)
        if current_path != root and (current_path / ".git").exists():
            directories[:] = []
            continue
        directories[:] = sorted(
            directory
            for directory in directories
            if directory not in EXCLUDED_WALK_DIRECTORIES
        )
        if basename in files:
            matches.append(current_path / basename)
    return sorted(matches)


def audit_dependabot(report: PostureReport, root: Path) -> None:
    text = _read_text(report, root, DEPENDABOT, "baseline.dependabot.coverage")
    if text is None:
        return
    declared = _dependabot_updates(text)
    expected = _manifest_directories(root)
    missing = sorted(expected - set(declared))
    duplicates = sorted(
        update for update in set(declared) if declared.count(update) > 1
    )
    for ecosystem, directory in missing:
        report.finding(
            "baseline.dependabot.coverage",
            f"missing Dependabot update for {ecosystem} at {directory}",
            path=DEPENDABOT.as_posix(),
        )
    for ecosystem, directory in duplicates:
        report.finding(
            "baseline.dependabot.coverage",
            f"duplicate Dependabot update for {ecosystem} at {directory}",
            path=DEPENDABOT.as_posix(),
        )
    if not missing and not duplicates:
        report.passed(
            "baseline.dependabot.coverage",
            f"Dependabot covers all {len(expected)} Cargo, Docker, and Actions roots",
        )


def _top_level_yaml_scalar(text: str, key: str) -> str | None:
    match = re.search(rf"(?m)^{re.escape(key)}:\s*(.+?)\s*$", text)
    return None if match is None else _strip_yaml_scalar(match.group(1))


def audit_release_contract(report: PostureReport, root: Path) -> None:
    workflow = _read_text(report, root, CI_WORKFLOW, "baseline.release-contract")
    release_source = _read_text(report, root, SOURCE_RELEASE, "baseline.release-contract")
    cargo = _load_toml(report, root, Path("Cargo.toml"), "baseline.release-contract")
    citation = _read_text(report, root, Path("CITATION.cff"), "baseline.release-contract")
    if workflow is None or release_source is None or cargo is None or citation is None:
        return
    problems: list[str] = []
    package = cargo.get("package")
    package = package if isinstance(package, dict) else {}
    if _top_level_yaml_scalar(citation, "version") != package.get("version"):
        problems.append("Cargo.toml and CITATION.cff versions differ")
    if _top_level_yaml_scalar(citation, "license") != package.get("license"):
        problems.append("Cargo.toml and CITATION.cff licenses differ")

    release_claims = _workflow_job_body(workflow, "release-claims")
    if release_claims is None:
        problems.append("CI has no release-claims job")
    else:
        for command in (
            "bash scripts/check_release_claims.sh",
            "python3 -m unittest -v tests.test_source_release",
            "python3 scripts/build_source_release.py --output",
            "python3 scripts/build_source_release.py --verify",
        ):
            if command not in release_claims:
                problems.append(f"release-claims omits {command!r}")

    release_package = _workflow_job_body(workflow, "release-package")
    if release_package is None:
        problems.append("CI has no release-package job")
    else:
        if _workflow_job_needs(release_package) != ["required-ci"]:
            problems.append("release-package does not depend only on required-ci")
        for contract in (
            "startsWith(github.ref, 'refs/tags/v')",
            "github.event_name == 'workflow_dispatch'",
            "python3 scripts/build_source_release.py --output",
            "python3 scripts/build_source_release.py --verify",
        ):
            if contract not in release_package:
                problems.append(f"release-package omits {contract!r}")

    contracts = _workflow_job_body(workflow, "contracts")
    if contracts is None:
        problems.append("CI has no contracts job")
    else:
        for command in (
            "python3 scripts/local_ci_posture.py --profile baseline --format text",
            "python3 -m unittest -v tests.test_contract_surfaces tests.test_local_ci_posture",
        ):
            if command not in contracts:
                problems.append(f"contracts job omits {command!r}")

    for path in RELEASE_SURFACE_PATHS:
        if f'"{path}"' not in release_source:
            problems.append(f"source-release contract does not require {path}")

    if problems:
        for problem in problems:
            report.finding(
                "baseline.release-contract",
                problem,
                path=CI_WORKFLOW.as_posix(),
            )
    else:
        report.passed(
            "baseline.release-contract",
            "release metadata, required aggregate, package gate, and release surfaces agree",
        )


def _summarize_process_output(completed: subprocess.CompletedProcess[str]) -> str:
    combined = "\n".join(part for part in (completed.stdout, completed.stderr) if part)
    lines = [line.strip() for line in combined.splitlines() if line.strip()]
    if not lines:
        return f"exit code {completed.returncode} with no output"
    summary = " | ".join(lines[:4])
    return summary[:1000] + ("..." if len(summary) > 1000 else "")


def _run_read_only(
    runner: Runner,
    command: Sequence[str],
    *,
    root: Path,
    timeout: int = 180,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    if Path(command[0]).name == "cargo":
        raise RuntimeError("local CI posture must never invoke cargo directly")
    return runner(
        list(command),
        cwd=root,
        text=True,
        capture_output=True,
        check=False,
        timeout=timeout,
        env=env,
    )


def _repository_status(runner: Runner, root: Path) -> str | None:
    try:
        completed = _run_read_only(
            runner,
            ["git", "status", "--porcelain=v1", "--untracked-files=all"],
            root=root,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return completed.stdout if completed.returncode == 0 else None


def _mirror_ignore(directory: str, names: list[str]) -> set[str]:
    current = Path(directory)
    ignored = {name for name in names if name in EXCLUDED_WALK_DIRECTORIES}
    for name in names:
        candidate = current / name
        if candidate.is_dir() and (candidate / ".git").exists():
            ignored.add(name)
    return ignored


def _copy_repository_mirror(root: Path, destination: Path) -> None:
    """Copy current inputs for analyzers which may ask Cargo for metadata."""

    shutil.copytree(
        root,
        destination,
        symlinks=True,
        ignore=_mirror_ignore,
    )


def _full_commands(
    root: Path,
    resolved: dict[str, str],
    *,
    cargo_deny_root: Path | None,
) -> list[tuple[str, list[str], Path]]:
    workflows = [str(path.relative_to(root)) for path in _workflow_paths(root)]
    commands: list[tuple[str, list[str], Path]] = []
    if "actionlint" in resolved:
        commands.append(
            ("actionlint", [resolved["actionlint"], *workflows], root)
        )
    if "zizmor" in resolved:
        commands.append(
            (
                "zizmor",
                [
                    resolved["zizmor"],
                    "--format",
                    "json-v1",
                    "--offline",
                    *workflows,
                ],
                root,
            )
        )
    if "gitleaks" in resolved:
        commands.append(
            (
                "gitleaks",
                [
                    resolved["gitleaks"],
                    "git",
                    "--no-banner",
                    "--redact",
                    "--log-opts=--all",
                    ".",
                ],
                root,
            )
        )
    if "git-sizer" in resolved:
        commands.append(
            ("git-sizer", [resolved["git-sizer"], "--verbose"], root)
        )
    if "cargo-audit" in resolved:
        for lockfile in _repository_files(root, "Cargo.lock"):
            commands.append(
                (
                    "cargo-audit",
                    [
                        resolved["cargo-audit"],
                        "audit",
                        "--no-fetch",
                        "--no-yanked",
                        "--file",
                        str(lockfile.relative_to(root)),
                        "--json",
                    ],
                    root,
                )
            )
    deny_config = root / "deny.toml"
    if (
        "cargo-deny" in resolved
        and deny_config.is_file()
        and cargo_deny_root is not None
    ):
        for manifest in _independent_cargo_manifests(root):
            commands.append(
                (
                    "cargo-deny",
                    [
                        resolved["cargo-deny"],
                        "--manifest-path",
                        str(manifest.relative_to(root)),
                        "--config",
                        "deny.toml",
                        "--offline",
                        "--locked",
                        "check",
                    ],
                    cargo_deny_root,
                )
            )
    return commands


def audit_full_tools(
    report: PostureReport,
    root: Path,
    *,
    runner: Runner,
    which: Which,
) -> None:
    resolved: dict[str, str] = {}
    for tool in FULL_TOOLS:
        path = which(tool)
        if path is None:
            report.missing(
                f"full.tool.{tool}",
                f"optional full-profile tool is not installed on PATH: {tool}",
            )
        else:
            resolved[tool] = path
            report.passed(f"full.tool.{tool}", f"found {tool} at {path}")
    if not (root / "deny.toml").is_file():
        report.missing(
            "full.config.cargo-deny",
            "deny.toml is absent; license/source/ban policy must be chosen explicitly",
        )
    if not resolved:
        return

    before = _repository_status(runner, root)
    if before is None:
        report.missing(
            "full.config.git-metadata",
            "full profile requires readable Git metadata for read-only state guarding",
        )
        return

    environment = os.environ.copy()
    environment.update(
        {
            "CARGO_NET_OFFLINE": "true",
            "GITLEAKS_ENABLE_UPLOAD": "false",
            "NO_COLOR": "1",
        }
    )
    with tempfile.TemporaryDirectory(prefix="ostadix-ci-posture-") as temporary:
        temporary_root = Path(temporary)
        environment["CARGO_TARGET_DIR"] = str(temporary_root / "cargo-target")
        cargo_deny_root: Path | None = None
        if "cargo-deny" in resolved and (root / "deny.toml").is_file():
            cargo_deny_root = temporary_root / "cargo-deny-worktree"
            try:
                _copy_repository_mirror(root, cargo_deny_root)
            except OSError as error:
                cargo_deny_root = None
                report.finding(
                    "full.run.cargo-deny",
                    f"could not create isolated analyzer mirror: {error}",
                )
        for tool, command, command_root in _full_commands(
            root,
            resolved,
            cargo_deny_root=cargo_deny_root,
        ):
            if tool == "cargo-deny" and not (root / "deny.toml").is_file():
                continue
            try:
                completed = _run_read_only(
                    runner,
                    command,
                    root=command_root,
                    timeout=300,
                    env=environment,
                )
            except (OSError, subprocess.SubprocessError) as error:
                report.finding(
                    f"full.run.{tool}",
                    f"read-only analyzer could not run: {error}",
                )
                continue
            if completed.returncode == 0:
                display_command = " ".join(
                    Path(part).name if index == 0 else part
                    for index, part in enumerate(command)
                )
                report.passed(
                    f"full.run.{tool}",
                    f"read-only analyzer passed: {display_command}",
                )
            else:
                summary = _summarize_process_output(completed)
                lower = summary.lower()
                if tool == "cargo-audit" and any(
                    marker in lower
                    for marker in ("advisory db", "advisory database", "no such file")
                ):
                    report.missing(
                        "full.config.cargo-audit-db",
                        f"cargo-audit --no-fetch has no usable local advisory database: {summary}",
                    )
                else:
                    report.finding(
                        f"full.run.{tool}",
                        f"read-only analyzer reported findings: {summary}",
                    )

    after = _repository_status(runner, root)
    if after is None:
        report.finding(
            "full.read-only-guard",
            "could not verify repository state after analyzer execution",
        )
    elif after != before:
        report.finding(
            "full.read-only-guard",
            "an analyzer changed tracked or untracked repository state",
        )
    else:
        report.passed(
            "full.read-only-guard",
            "repository status is byte-for-byte unchanged after full checks",
        )


def _github_repository(remote: str) -> str | None:
    remote = remote.strip()
    patterns = (
        r"https?://github\.com/(?P<slug>[^/\s]+/[^/\s]+?)(?:\.git)?$",
        r"ssh://git@github\.com/(?P<slug>[^/\s]+/[^/\s]+?)(?:\.git)?$",
        r"git@github\.com:(?P<slug>[^/\s]+/[^/\s]+?)(?:\.git)?$",
    )
    for pattern in patterns:
        match = re.fullmatch(pattern, remote)
        if match is not None:
            return match.group("slug")
    return None


def _github_get(
    runner: Runner,
    gh: str,
    root: Path,
    endpoint: str,
) -> tuple[object | None, str | None]:
    try:
        completed = _run_read_only(
            runner,
            [gh, "api", "--method", "GET", endpoint],
            root=root,
            timeout=60,
        )
    except (OSError, subprocess.SubprocessError) as error:
        return None, str(error)
    if completed.returncode != 0:
        return None, _summarize_process_output(completed)
    try:
        return json.loads(completed.stdout), None
    except json.JSONDecodeError as error:
        return None, f"GitHub returned invalid JSON: {error}"


def audit_github(
    report: PostureReport,
    root: Path,
    *,
    runner: Runner,
    which: Which,
) -> None:
    gh = which("gh")
    if gh is None:
        report.missing(
            "github.tool.gh",
            "--github requested but gh is not installed on PATH",
        )
        return
    repository = os.environ.get("GITHUB_REPOSITORY")
    if not repository:
        try:
            remote = _run_read_only(
                runner,
                ["git", "config", "--get", "remote.origin.url"],
                root=root,
                timeout=30,
            )
        except (OSError, subprocess.SubprocessError) as error:
            report.missing("github.config.repository", f"cannot read origin URL: {error}")
            return
        repository = _github_repository(remote.stdout) if remote.returncode == 0 else None
    if not repository:
        report.missing(
            "github.config.repository",
            "cannot derive an owner/repository slug from GITHUB_REPOSITORY or origin",
        )
        return

    repository_metadata, repository_error = _github_get(
        runner, gh, root, f"repos/{repository}"
    )
    branch = (
        repository_metadata.get("default_branch")
        if isinstance(repository_metadata, dict)
        else None
    )
    if not isinstance(branch, str) or not branch:
        report.missing(
            "github.config.branch",
            "cannot determine the repository default branch with read-only API: "
            f"{repository_error}",
        )
        return
    encoded_branch = quote(branch, safe="")

    actions, actions_error = _github_get(
        runner, gh, root, f"repos/{repository}/actions/permissions/workflow"
    )
    if actions_error is not None or not isinstance(actions, dict):
        report.missing(
            "github.actions.permissions",
            f"cannot inspect GitHub Actions defaults with read-only API: {actions_error}",
        )
    elif (
        actions.get("default_workflow_permissions") != "read"
        or actions.get("can_approve_pull_request_reviews") is not False
    ):
        report.finding(
            "github.actions.permissions",
            "GitHub Actions defaults are not read-only with PR approval disabled",
        )
    else:
        report.passed(
            "github.actions.permissions",
            "GitHub Actions defaults are read-only and cannot approve pull requests",
        )

    rules, rules_error = _github_get(
        runner, gh, root, f"repos/{repository}/rules/branches/{encoded_branch}"
    )
    protection, protection_error = _github_get(
        runner,
        gh,
        root,
        f"repos/{repository}/branches/{encoded_branch}/protection",
    )
    if (
        protection_error is not None
        and "branch not protected" in protection_error.lower()
    ):
        protection = {}
        protection_error = None

    rules_available = rules_error is None and isinstance(rules, list)
    protection_available = protection_error is None and isinstance(protection, dict)
    if not rules_available and not protection_available:
        report.missing(
            "github.branch.rules",
            "cannot inspect rulesets or branch protection with read-only API: "
            f"rulesets={rules_error}; protection={protection_error}",
        )
        return

    rule_types: set[str] = set()
    contexts: set[str] = set()
    if isinstance(rules, list):
        rule_types = {
            rule.get("type")
            for rule in rules
            if isinstance(rule, dict) and isinstance(rule.get("type"), str)
        }
        for rule in rules:
            if not isinstance(rule, dict) or rule.get("type") != "required_status_checks":
                continue
            parameters = rule.get("parameters")
            if not isinstance(parameters, dict):
                continue
            checks = parameters.get("required_status_checks")
            if isinstance(checks, list):
                for check in checks:
                    if isinstance(check, dict) and isinstance(check.get("context"), str):
                        contexts.add(check["context"])

    pull_request_required = "pull_request" in rule_types
    if isinstance(protection, dict):
        pull_request_required |= isinstance(
            protection.get("required_pull_request_reviews"), dict
        )
        status_checks = protection.get("required_status_checks")
        if isinstance(status_checks, dict):
            legacy_contexts = status_checks.get("contexts")
            if isinstance(legacy_contexts, list):
                contexts.update(
                    context for context in legacy_contexts if isinstance(context, str)
                )
            legacy_checks = status_checks.get("checks")
            if isinstance(legacy_checks, list):
                for check in legacy_checks:
                    if isinstance(check, dict) and isinstance(check.get("context"), str):
                        contexts.add(check["context"])

    problems = []
    if not pull_request_required:
        problems.append("effective rules do not require pull requests")
    if "Required CI" not in contexts:
        problems.append("effective rules do not require the stable Required CI check")
    if problems:
        if not rules_available or not protection_available:
            report.missing(
                "github.branch.rules",
                "cannot establish required protection from the available policy source(s): "
                + "; ".join(problems),
            )
        else:
            for problem in problems:
                report.finding("github.branch.rules", problem)
    else:
        report.passed(
            "github.branch.rules",
            f"{repository}:{branch} requires pull requests and Required CI",
        )


def audit_repository(
    root: Path = ROOT,
    *,
    profile: str = "baseline",
    include_github: bool = False,
    runner: Runner = _default_runner,
    which: Which = shutil.which,
) -> PostureReport:
    report = PostureReport(profile, include_github)
    workflows = _workflow_paths(root)
    if not workflows:
        report.finding(
            "baseline.workflows.present",
            "no GitHub Actions workflow files were found",
            path=WORKFLOW_DIRECTORY.as_posix(),
        )
    else:
        report.passed(
            "baseline.workflows.present",
            f"found {len(workflows)} GitHub Actions workflow(s)",
        )
        audit_action_pins(report, root, workflows)
        audit_workflow_safety(report, root, workflows)
    audit_required_wiring(report, root)
    audit_dependabot(report, root)
    audit_release_contract(report, root)
    if profile == "full":
        audit_full_tools(report, root, runner=runner, which=which)
    if include_github:
        audit_github(report, root, runner=runner, which=which)
    return report


def _render_text(report: PostureReport) -> str:
    lines = []
    for record in report.ordered_checks():
        location = ""
        if "path" in record:
            location = f" {record['path']}"
            if "line" in record:
                location += f":{record['line']}"
        lines.append(
            f"{str(record['status']).upper():7} {record['id']}{location} - {record['message']}"
        )
    document = report.document()
    summary = document["summary"]
    assert isinstance(summary, dict)
    lines.append(
        "local-ci-posture: "
        f"status={document['status']} profile={report.profile} "
        f"pass={summary['pass']} findings={summary['findings']} "
        f"missing={summary['missing']} exit={document['exit_code']}"
    )
    return "\n".join(lines)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Audit local and optionally remote CI posture without mutation."
    )
    parser.add_argument(
        "--profile",
        choices=("baseline", "full"),
        default="baseline",
        help="stdlib-only baseline or optional-tool full audit (default: baseline)",
    )
    parser.add_argument(
        "--format",
        choices=("text", "json"),
        default="text",
        dest="output_format",
        help="deterministic report format (default: text)",
    )
    parser.add_argument(
        "--github",
        action="store_true",
        help="read GitHub Actions defaults and effective branch rules through gh GET requests",
    )
    arguments = parser.parse_args(argv)
    report = audit_repository(
        profile=arguments.profile,
        include_github=arguments.github,
    )
    if arguments.output_format == "json":
        print(json.dumps(report.document(), sort_keys=True, indent=2))
    else:
        print(_render_text(report))
    return report.exit_code()


if __name__ == "__main__":
    raise SystemExit(main())
