"""Tests for the source-bound Olangc WASM release manifest."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "ostadix_wasm_release.py"
SPEC = importlib.util.spec_from_file_location("ostadix_wasm_release", SCRIPT)
if SPEC is None or SPEC.loader is None:  # pragma: no cover
    raise RuntimeError(f"cannot import {SCRIPT}")
WASM = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = WASM
SPEC.loader.exec_module(WASM)


def fixture_module() -> bytes:
    # Core v1 with an empty code section. The gate checks the module envelope,
    # bounded section lengths/order, and the presence of a code section.
    return b"\x00asm\x01\x00\x00\x00\x0a\x01\x00"


class WasmReleaseTests(unittest.TestCase):
    def fixture(self, root: Path):
        project = root / "project"
        (project / "src").mkdir(parents=True)
        (project / "Cargo.toml").write_text("[package]\nname='fixture'\n", encoding="utf-8")
        (project / "src/main.rs").write_text("fn main() {}\n", encoding="utf-8")
        artifact = root / "hello.wasm"
        artifact.write_bytes(fixture_module())
        source = root / "hello.O"
        source.write_text("O{2}\n", encoding="utf-8")
        generator = root / "olangc"
        generator.write_bytes(b"fixture-olangc")
        manifest = root / "manifest.json"
        common = [
            "--project",
            str(project),
            "--artifact",
            str(artifact),
            "--input",
            str(source),
            "--generator",
            str(generator),
            "--source-tree",
            "1" * 40,
            "--base-commit",
            "3" * 40,
            "--source-archive-sha256",
            "2" * 64,
        ]
        return project, artifact, source, generator, manifest, common

    def test_create_and_verify_bind_every_input(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            project, artifact, source, generator, manifest, common = self.fixture(root)
            self.assertEqual(
                WASM.main(
                    [
                        "create",
                        *common,
                        "--rust-toolchain",
                        "rustc 1.97.1 (fixture)",
                        "--output",
                        str(manifest),
                    ]
                ),
                0,
            )
            payload = json.loads(manifest.read_text(encoding="utf-8"))
            self.assertEqual(payload["schema"], WASM.SCHEMA)
            self.assertEqual(payload["artifact"]["path"], WASM.LOGICAL_ARTIFACT)
            self.assertEqual(payload["project"], WASM.project_identity(project))
            self.assertEqual(WASM.main(["verify", *common, "--manifest", str(manifest)]), 0)

            (project / "src/main.rs").write_text("fn main(){panic!()}\n", encoding="utf-8")
            self.assertEqual(WASM.main(["verify", *common, "--manifest", str(manifest)]), 1)

    def test_create_refuses_to_clobber_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            *_, manifest, common = self.fixture(root)
            manifest.write_text("existing\n", encoding="utf-8")
            self.assertEqual(
                WASM.main(
                    [
                        "create",
                        *common,
                        "--rust-toolchain",
                        "rustc 1.97.1 (fixture)",
                        "--output",
                        str(manifest),
                    ]
                ),
                1,
            )
            self.assertEqual(manifest.read_text(encoding="utf-8"), "existing\n")

    def test_project_rejects_target_output_and_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            project, *_ = self.fixture(root)
            (project / "target").mkdir()
            with self.assertRaisesRegex(WASM.WasmReleaseError, "target output"):
                WASM.project_identity(project)
            (project / "target").rmdir()
            (project / "unsafe").symlink_to(project / "Cargo.toml")
            with self.assertRaisesRegex(WASM.WasmReleaseError, "non-symlink"):
                WASM.project_identity(project)

    def test_module_validator_rejects_truncation_and_accepts_data_count_order(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            valid = root / "valid.wasm"
            valid.write_bytes(
                b"\x00asm\x01\x00\x00\x00"
                b"\x0c\x01\x00"  # data-count
                b"\x0a\x01\x00"  # code
                b"\x0b\x01\x00"  # data
            )
            self.assertGreater(WASM.validate_module(valid)["bytes"], 8)
            truncated = root / "truncated.wasm"
            truncated.write_bytes(b"\x00asm\x01\x00\x00\x00\x0a\x05\x00")
            with self.assertRaisesRegex(WASM.WasmReleaseError, "truncated"):
                WASM.validate_module(truncated)


if __name__ == "__main__":
    unittest.main()
