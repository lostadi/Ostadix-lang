#!/usr/bin/env python3

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


def _load():
    path = ROOT / "scripts/ostadix_hosted_live_release.py"
    spec = importlib.util.spec_from_file_location("hosted_live_release_boot_objects", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


RELEASE = _load()


class HostedLiveBootObjectSnapshotTests(unittest.TestCase):
    def git(self, repo: Path, *arguments: str) -> str:
        result = subprocess.run(
            ["git", "-C", str(repo), *arguments],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def repository(self, repo: Path) -> None:
        self.git(repo, "init", "-q")
        self.git(repo, "config", "user.email", "fixture@example.invalid")
        self.git(repo, "config", "user.name", "Fixture")
        for relative in RELEASE.REQUIRED_ARCHIVE_PATHS:
            path = repo / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"fixture {relative}\n", encoding="utf-8")
            if relative.startswith("scripts/") or relative.endswith(".sh"):
                path.chmod(0o755)
        (repo / "src/lib.rs").parent.mkdir(parents=True, exist_ok=True)
        (repo / "src/lib.rs").write_text("pub const O: u8 = 27;\n", encoding="utf-8")
        self.git(repo, "add", "--all")
        self.git(repo, "commit", "-q", "-m", "fixture")

    def test_store_archive_is_deterministic_and_bound_to_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo = root / "repo"
            repo.mkdir()
            self.repository(repo)
            snapshot = RELEASE.create_source_snapshot(repo, root / "source.tar")
            first_root = root / "first"
            second_root = root / "second"
            first_root.mkdir()
            second_root.mkdir()
            first = RELEASE.create_boot_object_snapshot(repo, snapshot, first_root)
            second = RELEASE.create_boot_object_snapshot(repo, snapshot, second_root)

            self.assertEqual(first.archive.read_bytes(), second.archive.read_bytes())
            self.assertEqual(first.archive_sha256, second.archive_sha256)
            self.assertEqual(first.summary["tree"], snapshot.tree)
            self.assertEqual(first.summary["commit"], snapshot.head)
            self.assertGreater(first.summary["binding_count"], 0)
            with tarfile.open(first.archive, "r:") as archive:
                names = archive.getnames()
                self.assertIn("index.bin", names)
                self.assertIn("objects/sha256", names)
                self.assertFalse(any(name.startswith("/") or ".." in name.split("/") for name in names))
                self.assertTrue(all(member.uid == 0 and member.gid == 0 for member in archive))
                self.assertTrue(all(member.mtime == RELEASE.SOURCE_DATE_EPOCH for member in archive))

    def test_transferred_store_verifies_against_archive_without_git_database(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo = root / "repo"
            repo.mkdir()
            self.repository(repo)
            snapshot = RELEASE.create_source_snapshot(repo, root / "source.tar")
            build_root = root / "build"
            build_root.mkdir()
            packaged = RELEASE.create_boot_object_snapshot(repo, snapshot, build_root)
            source = root / "extracted-source"
            store = root / "transferred-store"
            RELEASE._extract_regular_snapshot(snapshot.archive, source)
            store.mkdir()
            with tarfile.open(packaged.archive, "r:") as archive:
                archive.extractall(store, filter="data")
            result = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "scripts/ostadix_boot_objects.py"),
                    "verify",
                    "--store",
                    str(store),
                    "--commit",
                    snapshot.head,
                    "--tree",
                    snapshot.tree,
                    "--source-root",
                    str(source),
                    "--json",
                ],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            verified = json.loads(result.stdout)
            self.assertEqual(verified["root_sha256"], packaged.summary["root_sha256"])


if __name__ == "__main__":
    unittest.main()
