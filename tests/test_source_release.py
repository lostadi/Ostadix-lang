"""Regression tests for the deterministic source-release builder."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zipfile


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "build_source_release.py"
SPEC = importlib.util.spec_from_file_location("ostadix_source_release", SCRIPT)
if SPEC is None or SPEC.loader is None:  # pragma: no cover - import failure is fatal
    raise RuntimeError(f"cannot import {SCRIPT}")
release = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = release
SPEC.loader.exec_module(release)


class SourceReleaseTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self._git("init", "-q")
        self._git("config", "user.name", "Source Release Test")
        self._git("config", "user.email", "source-release@example.invalid")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _git(self, *arguments: str) -> str:
        environment = os.environ.copy()
        environment.update(
            {
                "GIT_AUTHOR_DATE": "2001-02-03T04:05:06+0000",
                "GIT_COMMITTER_DATE": "2001-02-03T04:05:06+0000",
            }
        )
        result = subprocess.run(
            ["git", "-C", os.fspath(self.repo), *arguments],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=environment,
        )
        return result.stdout.strip()

    def _write(self, relative: str, data: str | bytes, *, executable: bool = False) -> None:
        destination = self.repo / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(data, bytes):
            destination.write_bytes(data)
        else:
            destination.write_text(data, encoding="utf-8")
        if executable:
            destination.chmod(0o755)

    def _commit(self, files: dict[str, str | bytes] | None = None) -> str:
        contents = {
            "Cargo.toml": "[package]\nname = \"release-fixture\"\nversion = \"0.1.0\"\n",
            "README.md": "committed readme\n",
        }
        if files:
            contents.update(files)
        for path, data in contents.items():
            self._write(path, data)
        self._git("add", "-f", "--all")
        self._git("commit", "-q", "-m", "fixture")
        return self._git("rev-parse", "HEAD")

    def _build(self, name: str, **options: object):
        return release.build_release(
            self.repo,
            str(options.pop("ref", "HEAD")),
            self.root / name,
            **options,
        )

    def test_allowlist_includes_sources_and_excludes_release_debris(self) -> None:
        commit = self._commit(
            {
                ".github/workflows/ci.yml": "name: fixture\n",
                ".DS_Store": b"finder",
                ".ocore-repair-backups/run/typeck.rs": "backup\n",
                "assets/logo.bin": b"intentional asset",
                "backends/__pycache__/shim.pyc": b"bytecode",
                "backends/shim.py": "print('source')\n",
                "c_cpp/O": b"native executable",
                "c_cpp/src/eval.c": "int eval(void) { return 0; }\n",
                "c_cpp/src/eval.o": b"object file",
                "codebase_tape.md": "development transcript\n",
                "cvelist-our": "generated list\n",
                "docs/design.md": "design\n",
                "examples/demo.O": "text^(demo)_text\n",
                "examples/generated.html": "<p>generated</p>\n",
                "fuzz/fuzz_targets/parser.rs": "fn main() {}\n",
                "ocore/kernel/main.oc": "module kernel::main;\n",
                "one-off.patch": "diff --git a/a b/a\n",
                "scratch.txt": "not an allowlisted top-level surface\n",
                "scripts/tool.sh": "#!/bin/sh\nexit 0\n",
                "src/lib.rs": "pub fn fixture() {}\n",
                "tests/fixture.rs": "#[test] fn fixture() {}\n",
            }
        )
        result = self._build("source.zip")
        self.assertEqual(result.commit, commit)

        manifest = release.verify_archive(result.output)
        prefix = manifest["prefix"]
        with zipfile.ZipFile(result.output) as archive:
            names = set(archive.namelist())
            included = {
                "Cargo.toml",
                "README.md",
                ".github/workflows/ci.yml",
                "assets/logo.bin",
                "backends/shim.py",
                "c_cpp/src/eval.c",
                "docs/design.md",
                "examples/demo.O",
                "fuzz/fuzz_targets/parser.rs",
                "ocore/kernel/main.oc",
                "scripts/tool.sh",
                "src/lib.rs",
                "tests/fixture.rs",
            }
            excluded = {
                ".DS_Store",
                ".ocore-repair-backups/run/typeck.rs",
                "backends/__pycache__/shim.pyc",
                "c_cpp/O",
                "c_cpp/src/eval.o",
                "codebase_tape.md",
                "cvelist-our",
                "examples/generated.html",
                "one-off.patch",
                "scratch.txt",
            }
            for path in included:
                self.assertIn(f"{prefix}/{path}", names)
            for path in excluded:
                self.assertNotIn(f"{prefix}/{path}", names)

            manifest_bytes = archive.read(f"{prefix}/{release.MANIFEST_NAME}")
            embedded = json.loads(manifest_bytes)
            self.assertEqual(embedded["file_count"], len(included))
            checksums = archive.read(f"{prefix}/{release.CHECKSUMS_NAME}").decode()
            cargo_digest = hashlib.sha256(
                archive.read(f"{prefix}/Cargo.toml")
            ).hexdigest()
            self.assertIn(f"{cargo_digest}  Cargo.toml\n", checksums)
            self.assertIn(
                f"{hashlib.sha256(manifest_bytes).hexdigest()}  {release.MANIFEST_NAME}\n",
                checksums,
            )

    def test_same_commit_produces_byte_identical_zip_across_ref_aliases(self) -> None:
        commit = self._commit(
            {
                "assets/data.bin": b"\x00\x01\x02",
                "scripts/release-helper.sh": "#!/bin/sh\nexit 0\n",
                "src/lib.rs": "pub const ANSWER: u8 = 42;\n",
            }
        )
        first = self._build("first.zip", ref="HEAD")
        second = self._build("second.zip", ref=commit)
        self.assertEqual(first.prefix, second.prefix)
        self.assertEqual(first.archive_sha256, second.archive_sha256)
        self.assertEqual(first.output.read_bytes(), second.output.read_bytes())

    def test_dirty_tree_requires_override_and_override_uses_commit_bytes(self) -> None:
        self._commit({"src/lib.rs": "pub fn clean() {}\n"})
        self._write("README.md", "uncommitted readme\n")
        self._write("untracked.txt", "untracked\n")

        with self.assertRaisesRegex(release.ReleaseError, "working tree is dirty"):
            self._build("refused.zip")

        result = self._build("allowed.zip", allow_dirty=True)
        manifest = release.verify_archive(result.output)
        with zipfile.ZipFile(result.output) as archive:
            payload = archive.read(f"{manifest['prefix']}/README.md")
            self.assertEqual(payload, b"committed readme\n")
            self.assertNotIn(
                f"{manifest['prefix']}/untracked.txt", set(archive.namelist())
            )

    def test_verifier_rejects_payload_tampering(self) -> None:
        self._commit({"src/lib.rs": "pub fn intact() {}\n"})
        result = self._build("valid.zip")
        tampered = self.root / "tampered.zip"

        with zipfile.ZipFile(result.output, "r") as source:
            members = [
                (info.filename, f"{(info.external_attr >> 16) & 0xFFFF:06o}", source.read(info))
                for info in source.infolist()
            ]
        with zipfile.ZipFile(
            tampered,
            "w",
            compression=zipfile.ZIP_DEFLATED,
            compresslevel=9,
        ) as destination:
            for name, mode, data in members:
                if name.endswith("/README.md"):
                    data = b"tampered\n"
                destination.writestr(
                    release._zip_info(name, mode),
                    data,
                    compress_type=zipfile.ZIP_DEFLATED,
                    compresslevel=9,
                )

        with self.assertRaisesRegex(release.ReleaseError, "payload does not match manifest"):
            release.verify_archive(tampered)


if __name__ == "__main__":
    unittest.main()
