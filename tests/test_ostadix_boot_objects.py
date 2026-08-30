#!/usr/bin/env python3

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/ostadix_boot_objects.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("ostadix_boot_objects", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


BOOT_OBJECTS = _load_module()


class BootObjectStoreTests(unittest.TestCase):
    def git(self, root: Path, *arguments: str, binary: bool = False):
        result = subprocess.run(
            ["git", "-C", str(root), *arguments],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=not binary,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout if binary else result.stdout.strip()

    def repository(self, root: Path) -> None:
        self.git(root, "init", "-q")
        self.git(root, "config", "user.email", "fixture@example.invalid")
        self.git(root, "config", "user.name", "Fixture")
        (root / "README.md").write_text("same payload\n", encoding="utf-8")
        duplicate = root / "docs" / "copy.txt"
        duplicate.parent.mkdir()
        duplicate.write_text("same payload\n", encoding="utf-8")
        tool = root / "bin" / "ostadix-tool"
        tool.parent.mkdir()
        tool.write_text("#!/bin/sh\nprintf 'boot objects\\n'\n", encoding="utf-8")
        tool.chmod(0o755)
        (root / "empty").write_bytes(b"")
        self.git(root, "add", "--all")
        self.git(root, "commit", "-q", "-m", "fixture")

    def archive(self, repo: Path, tree: str, destination: Path) -> None:
        archive = destination.parent / f"{destination.name}.tar"
        self.git(repo, "archive", "--format=tar", "--output", str(archive), tree)
        destination.mkdir()
        with tarfile.open(archive, "r:") as source:
            source.extractall(destination, filter="data")

    def run_cli(self, *arguments: object) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), *(str(value) for value in arguments)],
            cwd=ROOT,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def test_index_matches_rust_v1_golden_vector(self):
        raw_sha256 = bytes.fromhex(
            "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881"
        )
        git_sha1 = bytes.fromhex("c1b0730e0133447badcfd47fd144e254807b06e1")
        model = BOOT_OBJECTS.SourceModel(
            commit_sha1=bytes.fromhex("11" * 20),
            tree_sha1=bytes.fromhex("22" * 20),
            objects=(BOOT_OBJECTS.ObjectRecord(raw_sha256, git_sha1, 1),),
            bindings=(
                BOOT_OBJECTS.BindingRecord("x", b"x", 0o100644, raw_sha256),
            ),
            object_data={raw_sha256: b"x"},
            logical_bytes=1,
            stored_bytes=1,
        )
        expected = bytes.fromhex(
            "4f424f494458000000010050000000d3"
            + "11" * 20
            + "22" * 20
            + "000000010000000100000000000000010000000000000001"
            + "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881"
            + "c1b0730e0133447badcfd47fd144e254807b06e1"
            + "0000000000000001"
            + "0001000081a4"
            + "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881"
            + "78"
            + "448dd2f12e496130e51201f9925f7edc46b5a0febfa19b74eaa664e2c1401e5b"
        )
        encoded = BOOT_OBJECTS.encode_index(model)
        self.assertEqual(len(encoded), 211)
        self.assertEqual(encoded, expected)
        self.assertEqual(BOOT_OBJECTS.parse_index(encoded).domain_digest.hex(), expected[-32:].hex())

    def test_build_is_deterministic_deduplicated_and_machine_inspectable(self):
        with tempfile.TemporaryDirectory() as temporary_raw:
            temporary = Path(temporary_raw)
            repo = temporary / "repo"
            repo.mkdir()
            self.repository(repo)
            commit = self.git(repo, "rev-parse", "HEAD")
            tree = self.git(repo, "rev-parse", "HEAD^{tree}")
            source = temporary / "source"
            self.archive(repo, tree, source)
            first = temporary / "first-store"
            second = temporary / "second-store"

            first_result = self.run_cli(
                "build",
                "--repo",
                repo,
                "--commit",
                commit,
                "--tree",
                tree,
                "--source-root",
                source,
                "--output",
                first,
                "--json",
            )
            self.assertEqual(first_result.returncode, 0, first_result.stderr)
            first_json = json.loads(first_result.stdout)
            self.assertTrue(first_json["ok"])
            self.assertEqual(first_json["binding_count"], 4)
            self.assertEqual(first_json["unique_object_count"], 3)
            self.assertLess(first_json["stored_bytes"], first_json["logical_bytes"])

            second_json = BOOT_OBJECTS.build_store(repo, commit, tree, source, second)
            self.assertTrue(second_json["ok"])
            self.assertEqual((first / "index.bin").read_bytes(), (second / "index.bin").read_bytes())
            self.assertEqual(sorted(path.name for path in first.iterdir()), ["index.bin", "objects"])
            first_cas = first / "objects" / "sha256"
            second_cas = second / "objects" / "sha256"
            self.assertEqual(
                sorted(path.name for path in first_cas.iterdir()),
                sorted(path.name for path in second_cas.iterdir()),
            )

            index_bytes = (first / "index.bin").read_bytes()
            self.assertEqual(index_bytes[:8], b"OBOIDX\0\0")
            self.assertEqual(int.from_bytes(index_bytes[8:10], "big"), 1)
            self.assertEqual(int.from_bytes(index_bytes[10:12], "big"), 80)
            self.assertEqual(int.from_bytes(index_bytes[12:16], "big"), len(index_bytes))
            self.assertEqual(
                index_bytes[-32:],
                hashlib.sha256(BOOT_OBJECTS.INDEX_DIGEST_DOMAIN + index_bytes[:-32]).digest(),
            )

            inspection = self.run_cli("inspect", "--store", first, "--full", "--json")
            self.assertEqual(inspection.returncode, 0, inspection.stderr)
            inspected = json.loads(inspection.stdout)
            self.assertEqual(inspected["git_tree_sha1"], tree)
            self.assertEqual(
                [binding["path"] for binding in inspected["bindings"]],
                ["README.md", "bin/ostadix-tool", "docs/copy.txt", "empty"],
            )
            executable = next(
                binding for binding in inspected["bindings"] if binding["path"] == "bin/ostadix-tool"
            )
            self.assertEqual(executable["mode"], "100755")
            self.assertEqual(inspected["commit"], commit)
            self.assertEqual(inspected["tree"], tree)
            self.assertEqual(inspected["object_count"], 3)
            self.assertEqual(inspected["index_root_sha256"], index_bytes[-32:].hex())

    def test_staged_tree_is_independent_of_recorded_base_commit(self):
        with tempfile.TemporaryDirectory() as temporary_raw:
            temporary = Path(temporary_raw)
            repo = temporary / "repo"
            repo.mkdir()
            self.repository(repo)
            base_commit = self.git(repo, "rev-parse", "HEAD")
            (repo / "README.md").write_text("staged but uncommitted\n", encoding="utf-8")
            self.git(repo, "add", "README.md")
            staged_tree = self.git(repo, "write-tree")
            self.assertNotEqual(staged_tree, self.git(repo, "rev-parse", "HEAD^{tree}"))
            source = temporary / "source"
            self.archive(repo, staged_tree, source)
            store = temporary / "store"

            BOOT_OBJECTS.build_store(repo, base_commit, staged_tree, source, store)
            parsed, _ = BOOT_OBJECTS.verify_store(store)
            self.assertEqual(parsed.commit_sha1.hex(), base_commit)
            self.assertEqual(parsed.tree_sha1.hex(), staged_tree)
            verified = self.run_cli(
                "verify",
                "--store",
                store,
                "--repo",
                repo,
                "--commit",
                base_commit,
                "--tree",
                staged_tree,
                "--source-root",
                source,
                "--json",
            )
            self.assertEqual(verified.returncode, 0, verified.stderr)
            self.assertEqual(json.loads(verified.stdout)["operation"], "verify")

            database_free = self.run_cli(
                "verify",
                "--store",
                store,
                "--commit",
                base_commit,
                "--tree",
                staged_tree,
                "--source-root",
                source,
                "--json",
            )
            self.assertEqual(database_free.returncode, 0, database_free.stderr)
            self.assertEqual(json.loads(database_free.stdout)["tree"], staged_tree)

            (source / "README.md").write_text("tampered after store build\n", encoding="utf-8")
            rejected = self.run_cli(
                "verify",
                "--store",
                store,
                "--commit",
                base_commit,
                "--tree",
                staged_tree,
                "--source-root",
                source,
                "--json",
            )
            self.assertEqual(rejected.returncode, 1)
            self.assertFalse(json.loads(rejected.stdout)["ok"])

    def test_source_root_must_be_an_exact_archive_without_extras(self):
        with tempfile.TemporaryDirectory() as temporary_raw:
            temporary = Path(temporary_raw)
            repo = temporary / "repo"
            repo.mkdir()
            self.repository(repo)
            commit = self.git(repo, "rev-parse", "HEAD")
            source = temporary / "source"
            self.archive(repo, commit, source)
            (source / "untracked.txt").write_text("must be rejected\n", encoding="utf-8")
            with self.assertRaisesRegex(BOOT_OBJECTS.BootObjectError, "not the exact Git tree"):
                BOOT_OBJECTS.build_store(
                    repo,
                    commit,
                    None,
                    source,
                    temporary / "store",
                )

    def test_git_symlink_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary_raw:
            temporary = Path(temporary_raw)
            repo = temporary / "repo"
            repo.mkdir()
            self.repository(repo)
            os.symlink("README.md", repo / "linked")
            self.git(repo, "add", "linked")
            self.git(repo, "commit", "-q", "-m", "symlink")
            commit = self.git(repo, "rev-parse", "HEAD")
            source = temporary / "source"
            source.mkdir()
            with self.assertRaisesRegex(BOOT_OBJECTS.BootObjectError, "accepts only regular blobs"):
                BOOT_OBJECTS.load_source_model(repo, commit, None, source)

    def test_verifier_rejects_cas_tampering(self):
        with tempfile.TemporaryDirectory() as temporary_raw:
            temporary = Path(temporary_raw)
            repo = temporary / "repo"
            repo.mkdir()
            self.repository(repo)
            commit = self.git(repo, "rev-parse", "HEAD")
            source = temporary / "source"
            self.archive(repo, commit, source)
            store = temporary / "store"
            BOOT_OBJECTS.build_store(repo, commit, None, source, store)
            object_path = next((store / "objects" / "sha256").iterdir())
            object_path.chmod(0o644)
            object_path.write_bytes(object_path.read_bytes() + b"tampered")

            with self.assertRaisesRegex(BOOT_OBJECTS.BootObjectError, "length mismatch"):
                BOOT_OBJECTS.verify_store(store)
            cli = self.run_cli("verify", "--store", store, "--json")
            self.assertEqual(cli.returncode, 1)
            failure = json.loads(cli.stdout)
            self.assertFalse(failure["ok"])
            self.assertEqual(failure["operation"], "verify")

    def test_output_is_no_clobber(self):
        with tempfile.TemporaryDirectory() as temporary_raw:
            temporary = Path(temporary_raw)
            repo = temporary / "repo"
            repo.mkdir()
            self.repository(repo)
            commit = self.git(repo, "rev-parse", "HEAD")
            source = temporary / "source"
            self.archive(repo, commit, source)
            output = temporary / "existing"
            output.mkdir()
            marker = output / "keep"
            marker.write_text("preserved\n", encoding="utf-8")

            with self.assertRaisesRegex(BOOT_OBJECTS.BootObjectError, "refusing to replace"):
                BOOT_OBJECTS.build_store(repo, commit, None, source, output)
            self.assertEqual(marker.read_text(encoding="utf-8"), "preserved\n")


if __name__ == "__main__":
    unittest.main()
