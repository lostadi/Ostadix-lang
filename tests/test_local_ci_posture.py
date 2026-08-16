from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "local_ci_posture.py"
SPEC = importlib.util.spec_from_file_location("ostadix_local_ci_posture", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
posture = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = posture
SPEC.loader.exec_module(posture)


class LocalCiPostureTests(unittest.TestCase):
    def run_posture(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["python3", "-B", str(SCRIPT), *arguments],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def copy_baseline_fixture(self, destination: Path) -> None:
        for relative in (
            ".github/dependabot.yml",
            ".github/workflows/ci.yml",
            ".github/workflows/fuzz.yml",
            "CITATION.cff",
            "Cargo.lock",
            "Cargo.toml",
            "Dockerfile",
            "ci/required-jobs.toml",
            "ci/test-suites.toml",
            "docs/CI_POSTURE.md",
            "scripts/build_source_release.py",
        ):
            source = ROOT / relative
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)

        # Dependabot coverage includes these independent Cargo roots.
        for relative in (
            "fuzz/Cargo.toml",
            "mcp/ostadix_lang_mcp_server/Cargo.toml",
        ):
            source = ROOT / relative
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)

    @staticmethod
    def finding_ids(report: object) -> set[str]:
        return {
            str(record["id"])
            for record in report.ordered_checks()
            if record["status"] == "finding"
        }

    def test_checked_in_baseline_passes(self) -> None:
        result = self.run_posture("--profile", "baseline", "--format", "text")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(
            "local-ci-posture: status=pass profile=baseline",
            result.stdout,
        )
        self.assertNotIn("FINDING", result.stdout)
        self.assertNotIn("MISSING", result.stdout)

    def test_json_report_is_deterministic_and_versioned(self) -> None:
        first = self.run_posture("--profile", "baseline", "--format", "json")
        second = self.run_posture("--profile", "baseline", "--format", "json")
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(first.stdout, second.stdout)
        document = json.loads(first.stdout)
        self.assertEqual(document["schema"], "ostadix.local-ci-posture/v1")
        self.assertEqual(document["status"], "pass")
        self.assertEqual(document["exit_code"], 0)
        ordering = [
            (
                record["id"],
                record.get("path", ""),
                record.get("line", 0),
                record["message"],
            )
            for record in document["checks"]
        ]
        self.assertEqual(ordering, sorted(ordering))

    def test_baseline_reports_unpinned_action_risky_trigger_write_and_runner(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Path(temporary)
            self.copy_baseline_fixture(fixture)
            workflow = fixture / ".github/workflows/ci.yml"
            text = workflow.read_text(encoding="utf-8")
            text = text.replace(
                "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
                "actions/checkout@v7",
                1,
            )
            text = text.replace("  pull_request:\n", "  pull_request_target:\n", 1)
            text = text.replace("  contents: read\n", "  contents: write\n", 1)
            text = text.replace("runs-on: ubuntu-latest", "runs-on: self-hosted", 1)
            workflow.write_text(text, encoding="utf-8")

            report = posture.audit_repository(fixture)

        self.assertEqual(report.exit_code(), 1)
        self.assertTrue(
            {
                "baseline.actions.full-sha",
                "baseline.workflows.risky-triggers",
                "baseline.workflows.permissions",
                "baseline.workflows.self-hosted",
            }.issubset(self.finding_ids(report))
        )

    def test_inline_mapping_risky_trigger_is_a_finding(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Path(temporary)
            self.copy_baseline_fixture(fixture)
            workflow = fixture / ".github/workflows/fuzz.yml"
            text = workflow.read_text(encoding="utf-8")
            start = text.index("on:\n")
            end = text.index("\n\n", start)
            text = text[:start] + "on: {pull_request_target: {}, push: {}}" + text[end:]
            workflow.write_text(text, encoding="utf-8")

            report = posture.audit_repository(fixture)

        self.assertEqual(report.exit_code(), 1)
        self.assertIn(
            "baseline.workflows.risky-triggers",
            self.finding_ids(report),
        )

    def test_inline_write_permission_is_a_finding(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Path(temporary)
            self.copy_baseline_fixture(fixture)
            workflow = fixture / ".github/workflows/ci.yml"
            text = workflow.read_text(encoding="utf-8")
            text = text.replace(
                "permissions:\n  contents: read\n",
                "permissions: {contents: write, actions: read}\n",
                1,
            )
            workflow.write_text(text, encoding="utf-8")

            report = posture.audit_repository(fixture)

        self.assertEqual(report.exit_code(), 1)
        self.assertIn("baseline.workflows.permissions", self.finding_ids(report))

    def test_required_aggregate_drift_is_a_finding(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Path(temporary)
            self.copy_baseline_fixture(fixture)
            workflow = fixture / ".github/workflows/ci.yml"
            text = workflow.read_text(encoding="utf-8")
            text = text.replace("      - docker\n", "", 1)
            workflow.write_text(text, encoding="utf-8")

            report = posture.audit_repository(fixture)

        self.assertEqual(report.exit_code(), 1)
        self.assertIn("baseline.required-aggregate", self.finding_ids(report))

    def test_dependabot_must_cover_every_independent_cargo_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Path(temporary)
            self.copy_baseline_fixture(fixture)
            dependabot = fixture / ".github/dependabot.yml"
            text = dependabot.read_text(encoding="utf-8")
            text = text.replace(
                "  - package-ecosystem: cargo\n"
                "    directory: /fuzz\n"
                "    schedule:\n"
                "      interval: weekly\n"
                "    open-pull-requests-limit: 3\n",
                "",
            )
            dependabot.write_text(text, encoding="utf-8")

            report = posture.audit_repository(fixture)

        findings = [
            record
            for record in report.ordered_checks()
            if record["id"] == "baseline.dependabot.coverage"
            and record["status"] == "finding"
        ]
        self.assertTrue(any("cargo at /fuzz" in record["message"] for record in findings))

    def test_full_profile_returns_two_when_tools_or_policy_are_missing(self) -> None:
        report = posture.audit_repository(
            ROOT,
            profile="full",
            which=lambda _tool: None,
        )
        self.assertEqual(report.exit_code(), 2)
        missing_ids = {
            str(record["id"])
            for record in report.ordered_checks()
            if record["status"] == "missing"
        }
        self.assertEqual(
            {f"full.tool.{tool}" for tool in posture.FULL_TOOLS}
            | {"full.config.cargo-deny"},
            missing_ids,
        )

    def test_full_profile_runs_available_tools_before_returning_two(self) -> None:
        calls: list[list[str]] = []

        def runner(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
            calls.append(list(command))
            if Path(command[0]).name == "git":
                return subprocess.CompletedProcess(command, 0, stdout="", stderr="")
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

        with tempfile.TemporaryDirectory() as temporary:
            fixture = Path(temporary)
            self.copy_baseline_fixture(fixture)
            report = posture.audit_repository(
                fixture,
                profile="full",
                runner=runner,
                which=lambda tool: "/tools/actionlint" if tool == "actionlint" else None,
            )

        self.assertEqual(report.exit_code(), 2)
        self.assertTrue(
            any(Path(command[0]).name == "actionlint" for command in calls)
        )

    def test_full_profile_uses_direct_tools_and_isolates_cargo_deny(self) -> None:
        calls: list[tuple[list[str], dict[str, object]]] = []

        def runner(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
            calls.append((list(command), kwargs))
            if Path(command[0]).name == "git" and command[1:3] == ["status", "--porcelain=v1"]:
                return subprocess.CompletedProcess(command, 0, stdout=" M fixture\n", stderr="")
            if Path(command[0]).name == "cargo-deny":
                analyzer_root = Path(str(kwargs["cwd"]))
                self.assertFalse((analyzer_root / ".git").exists())
                self.assertFalse((analyzer_root / "target").exists())
            return subprocess.CompletedProcess(command, 0, stdout="[]\n", stderr="")

        with tempfile.TemporaryDirectory() as temporary:
            fixture = Path(temporary)
            self.copy_baseline_fixture(fixture)
            (fixture / "deny.toml").write_text("[advisories]\n", encoding="utf-8")
            # Full profile requires readable Git metadata. The runner supplies
            # the status response; this marker also models a repository root.
            (fixture / ".git").mkdir()
            (fixture / "target").mkdir()
            (fixture / "target" / "must-not-be-mirrored").write_text(
                "build output\n", encoding="utf-8"
            )
            report = posture.audit_repository(
                fixture,
                profile="full",
                runner=runner,
                which=lambda tool: f"/tools/{tool}",
            )

        self.assertEqual(report.exit_code(), 0, report.document())
        commands = [command for command, _ in calls]
        self.assertTrue(any(Path(command[0]).name == "cargo-audit" for command in commands))
        self.assertTrue(any(Path(command[0]).name == "cargo-deny" for command in commands))
        self.assertFalse(any(Path(command[0]).name == "cargo" for command in commands))
        self.assertTrue(
            all("shell" not in kwargs or kwargs["shell"] is False for _, kwargs in calls)
        )
        audit_commands = [
            command for command in commands if Path(command[0]).name == "cargo-audit"
        ]
        self.assertTrue(all("--no-fetch" in command for command in audit_commands))
        self.assertTrue(all("--no-yanked" in command for command in audit_commands))
        deny_commands = [
            command for command in commands if Path(command[0]).name == "cargo-deny"
        ]
        self.assertTrue(all("--offline" in command for command in deny_commands))
        self.assertTrue(all("--locked" in command for command in deny_commands))
        zizmor_commands = [
            command for command in commands if Path(command[0]).name == "zizmor"
        ]
        self.assertTrue(all("--offline" in command for command in zizmor_commands))
        self.assertTrue(all("json-v1" in command for command in zizmor_commands))
        gitleaks_commands = [
            command for command in commands if Path(command[0]).name == "gitleaks"
        ]
        self.assertTrue(all("--redact" in command for command in gitleaks_commands))
        deny_calls = [
            kwargs
            for command, kwargs in calls
            if Path(command[0]).name == "cargo-deny"
        ]
        self.assertTrue(deny_calls)
        self.assertTrue(
            all(Path(str(kwargs["cwd"])) != fixture for kwargs in deny_calls)
        )
        actionlint_commands = [
            command for command in commands if Path(command[0]).name == "actionlint"
        ]
        self.assertTrue(all("-no-color" not in command for command in actionlint_commands))

    def test_github_mode_uses_only_get_and_accepts_required_effective_rules(self) -> None:
        calls: list[list[str]] = []

        def runner(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
            calls.append(list(command))
            endpoint = command[-1]
            if endpoint == "repos/lostadi/Ostadix-lang":
                payload: object = {"default_branch": "master"}
            elif endpoint.endswith("actions/permissions/workflow"):
                payload: object = {
                    "default_workflow_permissions": "read",
                    "can_approve_pull_request_reviews": False,
                }
            elif "/rules/branches/" in endpoint:
                payload = [
                    {"type": "pull_request"},
                    {
                        "type": "required_status_checks",
                        "parameters": {
                            "required_status_checks": [{"context": "Required CI"}]
                        },
                    },
                ]
            elif endpoint.endswith("branches/master/protection"):
                payload = {}
            else:
                return subprocess.CompletedProcess(command, 1, stdout="", stderr="unexpected")
            return subprocess.CompletedProcess(
                command,
                0,
                stdout=json.dumps(payload),
                stderr="",
            )

        with tempfile.TemporaryDirectory() as temporary:
            fixture = Path(temporary)
            self.copy_baseline_fixture(fixture)
            with mock.patch.dict(
                os.environ,
                {
                    "GITHUB_REPOSITORY": "lostadi/Ostadix-lang",
                },
                clear=True,
            ):
                report = posture.audit_repository(
                    fixture,
                    include_github=True,
                    runner=runner,
                    which=lambda tool: "/tools/gh" if tool == "gh" else None,
                )

        self.assertEqual(report.exit_code(), 0, report.document())
        gh_calls = [command for command in calls if Path(command[0]).name == "gh"]
        self.assertEqual(len(gh_calls), 4)
        self.assertTrue(all(command[1:4] == ["api", "--method", "GET"] for command in gh_calls))

    def test_github_mode_accepts_legacy_branch_protection(self) -> None:
        calls: list[list[str]] = []

        def runner(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
            calls.append(list(command))
            endpoint = command[-1]
            if endpoint == "repos/lostadi/Ostadix-lang":
                payload: object = {"default_branch": "feature/protection"}
            elif endpoint.endswith("actions/permissions/workflow"):
                payload = {
                    "default_workflow_permissions": "read",
                    "can_approve_pull_request_reviews": False,
                }
            elif "/rules/branches/" in endpoint:
                payload = []
            elif endpoint.endswith("branches/feature%2Fprotection/protection"):
                payload = {
                    "required_pull_request_reviews": {
                        "required_approving_review_count": 1
                    },
                    "required_status_checks": {
                        "contexts": ["Required CI"],
                        "checks": [],
                    },
                }
            else:
                return subprocess.CompletedProcess(
                    command, 1, stdout="", stderr="unexpected"
                )
            return subprocess.CompletedProcess(
                command,
                0,
                stdout=json.dumps(payload),
                stderr="",
            )

        with tempfile.TemporaryDirectory() as temporary:
            fixture = Path(temporary)
            self.copy_baseline_fixture(fixture)
            with mock.patch.dict(
                os.environ,
                {"GITHUB_REPOSITORY": "lostadi/Ostadix-lang"},
                clear=True,
            ):
                report = posture.audit_repository(
                    fixture,
                    include_github=True,
                    runner=runner,
                    which=lambda tool: "/tools/gh" if tool == "gh" else None,
                )

        self.assertEqual(report.exit_code(), 0, report.document())
        gh_calls = [command for command in calls if Path(command[0]).name == "gh"]
        self.assertTrue(
            any("feature%2Fprotection" in command[-1] for command in gh_calls)
        )


if __name__ == "__main__":
    unittest.main()
