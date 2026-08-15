from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "contract_surfaces.py"


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
        result = self.run_contracts("required-executables", "--suite", "rust")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout.splitlines(),
            ["bash", "clang", "openssl", "python3", "sqlite3"],
        )

    def test_unknown_suite_fails_closed(self) -> None:
        result = self.run_contracts("required-executables", "--suite", "absent")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unknown CI suite", result.stderr)


if __name__ == "__main__":
    unittest.main()
