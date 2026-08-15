from __future__ import annotations

import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path


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
