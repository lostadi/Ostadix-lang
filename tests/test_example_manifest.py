"""Focused schema and evidence regressions for examples/manifest.json."""

from __future__ import annotations

import json
import os
from pathlib import Path
import tempfile
import time
import unittest

import tests.example_manifest as manifest


def entry(*, edition: str = "rust", classification: str = "unit") -> dict:
    expectation = (
        {"result": {"tag": "int", "value": 2}}
        if edition == "python"
        else {"patterns": ["2"]}
    )
    return {
        "path": "hello.O",
        "editions": [edition],
        "classification": classification,
        "requirements": {
            "backends": ["python"],
            "programs": ["python3"],
            "authorities": ["process"],
        },
        "expected": {edition: expectation},
    }


class ManifestFixture:
    def __init__(self, example: dict) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        (self.root / "examples").mkdir()
        (self.root / "examples/hello.O").write_text(
            "python^(\n__oval_result__ = 2\n)_python\n", encoding="utf-8"
        )
        self.write({"schema_version": 1, "examples": [example]})

    def write(self, payload: dict) -> None:
        (self.root / "examples/manifest.json").write_text(
            json.dumps(payload), encoding="utf-8"
        )

    def close(self) -> None:
        self.temporary.cleanup()


class ExampleManifestTests(unittest.TestCase):
    def assert_invalid(self, example: dict, pattern: str) -> None:
        fixture = ManifestFixture(example)
        try:
            with self.assertRaisesRegex(manifest.ManifestError, pattern):
                manifest.load_manifest(fixture.root)
        finally:
            fixture.close()

    def test_rust_result_oracle_is_rejected_instead_of_ignored(self) -> None:
        example = entry()
        example["expected"]["rust"] = {"result": {"tag": "int", "value": 2}}
        self.assert_invalid(example, "result is only supported by the Python")

    def test_unknown_host_authority_is_rejected(self) -> None:
        example = entry()
        example["requirements"]["authorities"] = ["root-everything"]
        self.assert_invalid(example, "unknown host requirements")

    def test_unknown_entry_field_is_rejected(self) -> None:
        example = entry()
        example["timeout_second"] = 99
        self.assert_invalid(example, "unknown fields.*timeout_second")

    def test_unknown_top_level_field_is_rejected(self) -> None:
        fixture = ManifestFixture(entry())
        try:
            fixture.write(
                {
                    "schema_version": 1,
                    "examples": [entry()],
                    "edition": "rust",
                }
            )
            with self.assertRaisesRegex(manifest.ManifestError, "unknown fields.*edition"):
                manifest.load_manifest(fixture.root)
        finally:
            fixture.close()

    def test_requirement_file_must_not_escape_repository(self) -> None:
        example = entry()
        example["requirements"]["files"] = ["../outside"]
        self.assert_invalid(example, "normalized paths below the repository root")

    def test_opt_in_and_python_import_names_are_validated(self) -> None:
        malformed_opt_in = entry()
        malformed_opt_in["requirements"]["opt_in"] = ["NOT_AN_ASSIGNMENT"]
        self.assert_invalid(malformed_opt_in, "malformed assignment")

        malformed_package = entry()
        malformed_package["requirements"]["python_packages"] = ["pkg;exit()"]
        self.assert_invalid(malformed_package, "invalid import name")

    def test_all_skipped_interpreter_sweep_fails(self) -> None:
        fixture = ManifestFixture(entry(classification="manual"))
        original_root = manifest.ROOT
        try:
            manifest.ROOT = fixture.root
            result = manifest.run_interpreter_suite(
                "rust",
                fixture.root / "unused-runner",
                fixture.root / "unused-backends",
                {"unit"},
            )
            self.assertEqual(result, 1)
        finally:
            manifest.ROOT = original_root
            fixture.close()

    @unittest.skipUnless(os.name == "posix", "process-group evidence is POSIX")
    def test_timeout_kills_subprocess_descendants(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sentinel = root / "late-write"
            command = [
                "/bin/sh",
                "-c",
                f"(sleep 2; printf late > '{sentinel}') & wait",
            ]
            with self.assertRaises(manifest.CommandTimeout):
                manifest._run_command(
                    command,
                    cwd=root,
                    env=os.environ.copy(),
                    timeout=1,
                )
            time.sleep(1.25)
            self.assertFalse(sentinel.exists(), "timed-out descendant survived")

    def test_checked_in_manifest_has_executable_semantic_cases(self) -> None:
        examples = manifest.load_manifest()
        for edition in manifest.EDITIONS:
            semantic = [
                example
                for example in examples
                if edition in example["editions"]
                and example["classification"] in {"unit", "integration"}
                and "interpreter"
                in example["expected"][edition].get("modes", ["interpreter"])
            ]
            self.assertTrue(semantic, f"{edition} has no semantic interpreter cases")
        c17_aot = [
            example
            for example in examples
            if "c17" in example["editions"]
            and "aot" in example["expected"]["c17"].get("modes", [])
        ]
        self.assertGreaterEqual(len(c17_aot), 2)


if __name__ == "__main__":
    unittest.main()
