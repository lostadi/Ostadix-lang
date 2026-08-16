#!/usr/bin/env python3

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CHECKER = ROOT / "scripts" / "check_architecture_boundaries.py"


def run_checker(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(CHECKER), "--root", str(root)],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def write_minimal_tree(root: Path) -> None:
    for relative in (
        "src/parser.rs",
        "src/syntax_dialect.rs",
        "src/ir.rs",
        "src/effects.rs",
        "src/dispatch_model.rs",
        "src/placement/mod.rs",
        "src/placement/projection.rs",
        "src/placement/protocol/mod.rs",
        "src/evidence/admit.rs",
        "src/evidence/analyze.rs",
        "src/evidence/fact.rs",
        "src/evidence/intent.rs",
        "src/evidence/profile.rs",
    ):
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("pub struct Boundary;\n", encoding="utf-8")


class ArchitectureBoundaryTests(unittest.TestCase):
    def test_current_tree_respects_frozen_boundaries(self) -> None:
        result = run_checker(ROOT)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "architecture dependency boundaries: PASS\n")

    def test_wrong_way_production_dependency_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/parser.rs").write_text(
                "use crate::ir::PlanNodeId;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("narrow dialect projection", result.stderr)

    def test_unit_test_import_does_not_define_production_geometry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/parser.rs").write_text(
                "pub struct Syntax;\n#[cfg(test)]\n"
                "mod tests { use crate::ir::PlanNodeId; }\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
