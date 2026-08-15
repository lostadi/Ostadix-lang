from __future__ import annotations

import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "contract_surfaces.py"
SPEC = importlib.util.spec_from_file_location("ostadix_contract_surfaces", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
contracts = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(contracts)


class ContractSurfacesTests(unittest.TestCase):
    def run_contracts(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["python3", str(SCRIPT), *arguments],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_repository_contract_surfaces_are_consistent(self) -> None:
        result = self.run_contracts("validate")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "contract-surfaces: ok")

    def test_rust_suite_projects_openssl_from_one_manifest(self) -> None:
        result = self.run_contracts("required-executables", "--suite", "rust-hosted")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout.splitlines(),
            ["bash", "clang", "openssl", "python3", "sqlite3"],
        )

    def test_openssl_probe_uses_the_authoritative_version_subcommand(self) -> None:
        probe = contracts.runtime_probes()["openssl"]
        self.assertEqual(probe, {"executable": "openssl", "probe_args": ["version"]})
        completed = subprocess.CompletedProcess(
            ["/usr/bin/openssl", "version"],
            0,
            stdout="OpenSSL fixture\n",
        )
        with (
            mock.patch.object(contracts.shutil, "which", return_value="/usr/bin/openssl"),
            mock.patch.object(contracts.subprocess, "run", return_value=completed) as run,
        ):
            evidence = contracts.probe_runtime("openssl", probe)

        self.assertIn("runtime=openssl", evidence)
        self.assertIn("version=OpenSSL fixture", evidence)
        self.assertEqual(run.call_args.args[0], ["/usr/bin/openssl", "version"])

    def test_missing_runtime_probe_fails_with_typed_evidence(self) -> None:
        with mock.patch.object(contracts.shutil, "which", return_value=None):
            with self.assertRaisesRegex(
                contracts.ContractError,
                "status=missing-required.*runtime=python3",
            ):
                contracts.probe_runtime(
                    "python3",
                    {"executable": "python3", "probe_args": ["--version"]},
                )

    def test_mcp_toolchain_installs_clippy_before_invoking_it(self) -> None:
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        match = contracts.workflow_job_body(workflow, "mcp")
        self.assertIn("components: clippy", match)
        self.assertIn("cargo +1.97.1 clippy", match)
        self.assertLess(
            match.index("components: clippy"),
            match.index("cargo +1.97.1 clippy"),
        )

    def test_docker_smoke_is_an_independent_required_lane(self) -> None:
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        job = contracts.workflow_job_body(workflow, "docker")
        self.assertIn("runs-on: ubuntu-latest", job)
        self.assertIn(
            "python3 scripts/contract_surfaces.py probe-runtimes --suite docker",
            job,
        )
        self.assertIn("run: bash scripts/smoke-docker.sh", job)
        self.assertNotIn("actions/cache", job)
        self.assertNotIn("cargo ", job)

        required = contracts.load_toml(ROOT / "ci/required-jobs.toml")
        self.assertIn("docker", required["required_jobs"])
        self.assertIn(
            "docker",
            contracts.workflow_job_needs(workflow, "required-ci"),
        )
        aggregate = contracts.workflow_job_body(workflow, "required-ci")
        self.assertIn("DOCKER: ${{ needs.docker.result }}", aggregate)
        self.assertRegex(aggregate, r"for name in .*\bDOCKER\b.*; do")

    def test_unknown_suite_fails_closed(self) -> None:
        result = self.run_contracts("required-executables", "--suite", "absent")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unknown CI suite", result.stderr)

    def test_required_aggregate_needs_are_parsed_as_an_exact_block_list(self) -> None:
        workflow = """\
jobs:
  alpha:
    runs-on: ubuntu-latest
  required-ci:
    needs:
      - alpha
      - beta
    runs-on: ubuntu-latest
  beta:
    runs-on: ubuntu-latest
"""
        self.assertEqual(
            contracts.workflow_job_needs(workflow, "required-ci"),
            ["alpha", "beta"],
        )


if __name__ == "__main__":
    unittest.main()
