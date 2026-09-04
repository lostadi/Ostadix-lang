"""Security and determinism tests for the bounded per-host offline kit."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile


PROJECT_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = PROJECT_ROOT / "scripts" / "build_offline_kit.py"
BOOTSTRAP = PROJECT_ROOT / "scripts" / "bootstrap_offline_kit.sh"
SPEC = importlib.util.spec_from_file_location("ostadix_offline_kit", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
offline = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = offline
SPEC.loader.exec_module(offline)


PACKAGE_CHECKSUM = "a" * 64


class OfflineKitTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.host = offline.detect_posix_host()
        self.toolchain = self.root / "toolchain-input"
        self.vendor = self.root / "vendor-input"
        self._make_toolchain(self.toolchain, self.host)
        self._make_vendor(self.vendor)
        self.source_entries = self._source_entries("1.97.1")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    @staticmethod
    def _write_executable(path: Path, text: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        path.chmod(0o755)

    def _make_toolchain(self, root: Path, host: str) -> None:
        self._write_executable(
            root / "bin" / "rustc",
            "#!/bin/sh\n"
            "if [ \"${1:-}\" = -vV ]; then\n"
            "  cat <<'EOF'\n"
            "rustc 1.97.1 (fixture 2026-01-01)\n"
            "binary: rustc\n"
            "commit-hash: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n"
            "commit-date: 2026-01-01\n"
            f"host: {host}\n"
            "release: 1.97.1\n"
            "LLVM version: fixture\n"
            "EOF\n"
            "elif [ \"${1:-}\" = --print ]; then\n"
            "  echo fixture-target-libdir\n"
            "else\n"
            "  exit 2\n"
            "fi\n",
        )
        self._write_executable(
            root / "bin" / "cargo",
            "#!/bin/sh\n"
            "if [ \"${1:-}\" = --version ]; then\n"
            "  echo 'cargo 1.97.1 (fixture 2026-01-01)'\n"
            "else\n"
            "  { pwd; env | sort; printf '%s\\n' \"$*\"; } "
            ">>\"$CARGO_HOME/fixture-invocations.log\"\n"
            "  exit 0\n"
            "fi\n",
        )
        self._write_executable(root / "bin" / "rustdoc", "#!/bin/sh\nexit 0\n")
        wasm = root / "lib" / "rustlib" / "wasm32-wasip1" / "lib"
        wasm.mkdir(parents=True)
        (wasm / "libstd-fixture.rlib").write_bytes(b"fixture wasm std\n")
        legal = {
            "share/doc/cargo/LICENSE-APACHE": "fixture Apache license\n",
            "share/doc/cargo/LICENSE-MIT": "fixture MIT license\n",
            "share/doc/rust/COPYRIGHT.html": "<p>fixture notices</p>\n",
        }
        for relative, text in legal.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")

    @staticmethod
    def _make_vendor(root: Path) -> None:
        crate = root / "demo-1.0.0"
        source = crate / "src" / "lib.rs"
        source.parent.mkdir(parents=True)
        source.write_text("pub fn demo() {}\n", encoding="utf-8")
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        checksum = {
            "$comment": (
                "This file only protects against accidental modifications. "
                "It is not a security mechanism and does not protect against "
                "malicious changes."
            ),
            "files": {"src/lib.rs": digest},
            "package": PACKAGE_CHECKSUM,
        }
        (crate / ".cargo-checksum.json").write_text(
            json.dumps(checksum, sort_keys=True, separators=(",", ":")),
            encoding="utf-8",
        )

    @staticmethod
    def _lockfile() -> bytes:
        return (
            "version = 4\n\n"
            "[[package]]\n"
            'name = "demo"\n'
            'version = "1.0.0"\n'
            'source = "registry+https://github.com/rust-lang/crates.io-index"\n'
            f'checksum = "{PACKAGE_CHECKSUM}"\n'
        ).encode()

    def _source_entries(self, release: str) -> list[object]:
        entries = [
            offline.BytesEntry(
                "source/rust-toolchain.toml",
                "100644",
                (
                    "[toolchain]\n"
                    f'channel = "{release}"\n'
                    'profile = "minimal"\n'
                    'components = ["rustfmt", "clippy"]\n'
                    'targets = ["wasm32-wasip1"]\n'
                ).encode(),
            ),
            offline.BytesEntry(
                "source/scripts/build_offline_kit.py", "100755", SCRIPT.read_bytes()
            ),
            offline.BytesEntry(
                "source/scripts/bootstrap_offline_kit.sh",
                "100755",
                BOOTSTRAP.read_bytes(),
            ),
        ]
        entries.extend(
            offline.BytesEntry(path, "100644", self._lockfile())
            for path in offline.RELEASED_CARGO_LOCKS
        )
        return entries

    def _build(self, name: str = "kit.zip", **overrides: object) -> Path:
        output = self.root / name
        arguments = {
            "source_entries": self.source_entries,
            "commit": "c" * 40,
            "toolchain": self.toolchain,
            "vendor": self.vendor,
            "output": output,
            "prefix": "Ostadix-offline-fixture",
        }
        arguments.update(overrides)
        offline.build_kit_from_entries(**arguments)
        return output

    @staticmethod
    def _extract_generated_zip(archive_path: Path, destination: Path) -> Path:
        with zipfile.ZipFile(archive_path) as archive:
            for info in archive.infolist():
                relative = offline._safe_relative(info.filename)
                target = destination.joinpath(*relative.parts)
                target.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(info) as source, target.open("xb") as output:
                    shutil.copyfileobj(source, output)
                target.chmod((info.external_attr >> 16) & 0o777)
        return destination / "Ostadix-offline-fixture"

    def test_build_is_deterministic_and_manifest_is_exact(self) -> None:
        first = self._build("first.zip")
        second = self._build("second.zip")
        self.assertEqual(first.read_bytes(), second.read_bytes())
        manifest = offline.verify_archive(first)
        self.assertEqual(manifest["toolchain"]["host"], self.host)
        self.assertEqual(manifest["toolchain"]["release"], "1.97.1")
        self.assertEqual(manifest["toolchain"]["wasm_target"], "wasm32-wasip1")
        self.assertEqual(manifest["vendor"]["crate_directories"], 1)
        self.assertEqual(set(manifest["profiles"]), set(offline.PROFILE_COMMANDS))
        self.assertEqual(manifest["nonclaims"], list(offline.NONCLAIMS))
        with zipfile.ZipFile(first) as archive:
            prefix = manifest["prefix"]
            self.assertEqual(
                archive.read(f"{prefix}/{offline.BOOTSTRAP_NAME}"),
                archive.read(f"{prefix}/{offline.SOURCE_BOOTSTRAP_NAME}"),
            )

    def test_top_level_bootstrap_is_bound_to_selected_source_revision(self) -> None:
        selected = b"#!/bin/sh\necho selected-revision\n"
        source_entries = [
            offline.BytesEntry(entry.path, entry.mode, selected)
            if entry.path == offline.SOURCE_BOOTSTRAP_NAME
            else entry
            for entry in self.source_entries
        ]
        archive_path = self._build(source_entries=source_entries)
        manifest = offline.verify_archive(archive_path)
        with zipfile.ZipFile(archive_path) as archive:
            self.assertEqual(
                archive.read(f"{manifest['prefix']}/{offline.BOOTSTRAP_NAME}"),
                selected,
            )

    def test_manifest_rejects_an_unbound_bootstrap_record(self) -> None:
        manifest = offline.verify_archive(self._build())
        changed = json.loads(json.dumps(manifest))
        source_record = next(
            record
            for record in changed["files"]
            if record["path"] == offline.SOURCE_BOOTSTRAP_NAME
        )
        source_record["sha256"] = "d" * 64
        with self.assertRaisesRegex(offline.OfflineKitError, "not bound"):
            offline._manifest_records(changed)

    def test_extraction_is_host_gated_idempotent_and_tamper_evident(self) -> None:
        archive = self._build()
        kit_root = self._extract_generated_zip(archive, self.root / "unpacked")
        destination = kit_root / ".offline"
        offline.extract_payloads(kit_root, destination, detected_host=self.host)
        offline.extract_payloads(kit_root, destination, detected_host=self.host)
        self.assertTrue((destination / "toolchain/bin/cargo").is_file())
        config = (destination / "cargo-home/config.toml").read_text(encoding="utf-8")
        self.assertIn("replace-with = \"vendored-sources\"", config)
        self.assertIn("offline = true", config)

        with (destination / "toolchain/bin/rustc").open("ab") as rustc:
            rustc.write(b"# tampered\n")
        with self.assertRaisesRegex(
            offline.OfflineKitError, "toolchain does not match kit seal"
        ):
            offline.extract_payloads(kit_root, destination, detected_host=self.host)

    def test_existing_extraction_rejects_legacy_cargo_configuration(self) -> None:
        archive = self._build()
        kit_root = self._extract_generated_zip(archive, self.root / "unpacked")
        destination = kit_root / ".offline"
        offline.extract_payloads(kit_root, destination, detected_host=self.host)
        (destination / "cargo-home/config").write_text(
            "[build]\nrustc-wrapper = 'attacker'\n", encoding="utf-8"
        )
        with self.assertRaisesRegex(offline.OfflineKitError, "forbidden legacy"):
            offline.extract_payloads(kit_root, destination, detected_host=self.host)

    def test_existing_extraction_rejects_mutable_directory_symlinks(self) -> None:
        for name in ("cargo-home", "target"):
            with self.subTest(name=name):
                archive = self._build(f"{name}.zip")
                kit_root = self._extract_generated_zip(
                    archive, self.root / f"unpacked-{name}"
                )
                destination = kit_root / ".offline"
                offline.extract_payloads(kit_root, destination, detected_host=self.host)
                path = destination / name
                if path.exists():
                    shutil.rmtree(path)
                outside = self.root / f"outside-{name}"
                outside.mkdir()
                path.symlink_to(outside, target_is_directory=True)
                with self.assertRaisesRegex(offline.OfflineKitError, "not a real directory"):
                    offline.extract_payloads(
                        kit_root, destination, detected_host=self.host
                    )

    def test_host_mismatch_fails_before_extraction(self) -> None:
        archive = self._build()
        kit_root = self._extract_generated_zip(archive, self.root / "unpacked")
        destination = kit_root / ".offline"
        wrong_host = next(host for host in offline.SUPPORTED_POSIX_HOSTS if host != self.host)
        with self.assertRaisesRegex(offline.OfflineKitError, "host mismatch"):
            offline.extract_payloads(kit_root, destination, detected_host=wrong_host)
        self.assertFalse(destination.exists())

    def test_existing_unsealed_destination_is_never_clobbered(self) -> None:
        archive = self._build()
        kit_root = self._extract_generated_zip(archive, self.root / "unpacked")
        destination = kit_root / ".offline"
        destination.mkdir()
        marker = destination / "keep.txt"
        marker.write_text("keep\n", encoding="utf-8")
        with self.assertRaisesRegex(offline.OfflineKitError, "unsealed"):
            offline.extract_payloads(kit_root, destination, detected_host=self.host)
        self.assertEqual(marker.read_text(encoding="utf-8"), "keep\n")

    def test_extracted_source_rejects_extra_cargo_config_and_symlink_directory(self) -> None:
        archive = self._build()
        kit_root = self._extract_generated_zip(archive, self.root / "unpacked")
        injected = kit_root / "source/.cargo/config.toml"
        injected.parent.mkdir(parents=True)
        injected.write_text(
            '[source.crates-io]\nreplace-with = "attacker"\n', encoding="utf-8"
        )
        with self.assertRaisesRegex(offline.OfflineKitError, "member closure"):
            offline.verify_extracted_kit(kit_root)

        injected.unlink()
        injected.parent.rmdir()
        outside = self.root / "outside-directory"
        outside.mkdir()
        (kit_root / "source/linked-directory").symlink_to(
            outside, target_is_directory=True
        )
        with self.assertRaisesRegex(offline.OfflineKitError, "symlinks are forbidden"):
            offline.verify_extracted_kit(kit_root)

    def test_existing_output_or_output_symlink_is_never_clobbered(self) -> None:
        existing = self.root / "existing.zip"
        existing.write_bytes(b"keep")
        with self.assertRaisesRegex(offline.OfflineKitError, "clobber"):
            self._build(output=existing)
        self.assertEqual(existing.read_bytes(), b"keep")

        target = self.root / "target.zip"
        link = self.root / "link.zip"
        link.symlink_to(target)
        with self.assertRaisesRegex(offline.OfflineKitError, "clobber"):
            self._build(output=link)
        self.assertTrue(link.is_symlink())
        self.assertFalse(target.exists())

    def test_payload_input_symlink_is_rejected(self) -> None:
        outside = self.root / "outside.txt"
        outside.write_text("outside\n", encoding="utf-8")
        (self.vendor / "demo-1.0.0" / "escape").symlink_to(outside)
        with self.assertRaisesRegex(offline.OfflineKitError, "symlinks are forbidden"):
            self._build()

    def test_tar_traversal_and_symlink_members_are_rejected(self) -> None:
        for name, member in (
            ("traversal", tarfile.TarInfo("toolchain/../../escape")),
            ("symlink", tarfile.TarInfo("toolchain/link")),
        ):
            with self.subTest(name=name):
                if name == "symlink":
                    member.type = tarfile.SYMTYPE
                    member.linkname = "outside"
                else:
                    member.type = tarfile.REGTYPE
                    member.size = 1
                member.uid = 0
                member.gid = 0
                member.mtime = 0
                member.mode = 0o644
                payload = self.root / f"{name}.tar.gz"
                with tarfile.open(payload, "w:gz") as archive:
                    archive.addfile(
                        member,
                        io.BytesIO(b"x") if member.isfile() else None,
                    )
                with self.assertRaises(offline.OfflineKitError):
                    offline._validate_tar(payload, "toolchain")

    def test_tar_root_file_and_file_directory_conflicts_are_rejected(self) -> None:
        for name, members in (
            ("root-file", [("toolchain", b"x")]),
            (
                "path-conflict",
                [("toolchain/bin", b"x"), ("toolchain/bin/rustc", b"y")],
            ),
        ):
            with self.subTest(name=name):
                payload = self.root / f"{name}.tar.gz"
                with tarfile.open(payload, "w:gz") as archive:
                    for member_name, data in members:
                        member = tarfile.TarInfo(member_name)
                        member.size = len(data)
                        member.mode = 0o644
                        member.uid = 0
                        member.gid = 0
                        member.mtime = 0
                        archive.addfile(member, io.BytesIO(data))
                with self.assertRaises(offline.OfflineKitError):
                    offline._validate_tar(payload, "toolchain")

    def test_zip_payload_tamper_is_rejected(self) -> None:
        source = self._build()
        tampered = self.root / "tampered.zip"
        with zipfile.ZipFile(source) as old, zipfile.ZipFile(tampered, "w") as new:
            for info in old.infolist():
                data = old.read(info)
                if info.filename.endswith("source/rust-toolchain.toml"):
                    data += b"# tamper\n"
                new.writestr(info, data)
        with self.assertRaises(offline.OfflineKitError):
            offline.verify_archive(tampered)

    def test_noncanonical_zip_timestamp_is_rejected(self) -> None:
        source = self._build()
        changed = self.root / "timestamp.zip"
        with zipfile.ZipFile(source) as old, zipfile.ZipFile(changed, "w") as new:
            for index, old_info in enumerate(old.infolist()):
                data = old.read(old_info)
                if index == 0:
                    info = zipfile.ZipInfo(old_info.filename, (2020, 1, 1, 0, 0, 0))
                    info.compress_type = old_info.compress_type
                    info.create_system = old_info.create_system
                    info.external_attr = old_info.external_attr
                else:
                    info = old_info
                new.writestr(info, data)
        with self.assertRaisesRegex(offline.OfflineKitError, "non-canonical ZIP"):
            offline.verify_archive(changed)

    def test_noncanonical_zip_internal_attributes_are_rejected(self) -> None:
        source = self._build()
        data = bytearray(source.read_bytes())
        with zipfile.ZipFile(source) as archive:
            central_offset = archive.start_dir
        self.assertEqual(data[central_offset : central_offset + 4], b"PK\x01\x02")
        data[central_offset + 36 : central_offset + 38] = b"\x01\x00"
        changed = self.root / "internal-attributes.zip"
        changed.write_bytes(data)
        with self.assertRaisesRegex(offline.OfflineKitError, "non-canonical ZIP"):
            offline.verify_archive(changed)

    def test_prepended_or_trailing_zip_bytes_are_rejected(self) -> None:
        source = self._build()
        for name, data in (
            ("prepended", b"x" + source.read_bytes()),
            ("trailing", source.read_bytes() + b"x"),
        ):
            with self.subTest(name=name):
                changed = self.root / f"{name}.zip"
                changed.write_bytes(data)
                with self.assertRaisesRegex(offline.OfflineKitError, "non-canonical ZIP"):
                    offline.verify_archive(changed)

    def test_source_toolchain_and_vendor_lock_closure_must_match(self) -> None:
        mismatched_source = self._source_entries("1.96.0")
        with self.assertRaisesRegex(offline.OfflineKitError, "channel must equal"):
            self._build(source_entries=mismatched_source)

        (self.vendor / "demo-1.0.0/.cargo-checksum.json").unlink()
        with self.assertRaisesRegex(offline.OfflineKitError, "lacks"):
            self._build(name="missing-vendor.zip")

    def test_vendor_checksum_schema_rejects_unknown_or_non_text_comment(self) -> None:
        checksum_path = self.vendor / "demo-1.0.0/.cargo-checksum.json"
        original = json.loads(checksum_path.read_text(encoding="utf-8"))
        for name, value in (("unknown", True), ("$comment", 7)):
            with self.subTest(name=name):
                changed = dict(original)
                changed[name] = value
                checksum_path.write_text(
                    json.dumps(changed, sort_keys=True, separators=(",", ":")),
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(offline.OfflineKitError, "malformed"):
                    self._build(name=f"bad-vendor-{name}.zip")

    def test_bootstrap_uses_kit_root_and_frozen_local_cargo(self) -> None:
        text = BOOTSTRAP.read_text(encoding="utf-8")
        self.assertIn('dirname -- "$0")" && pwd -P', text)
        self.assertNotIn('dirname -- "$0")/..', text)
        self.assertIn('PATH="$OFFLINE_ROOT/toolchain/bin:$PATH"', text)
        self.assertIn('CARGO_HOME="$OFFLINE_ROOT/cargo-home"', text)
        self.assertIn('CARGO_TARGET_DIR="$OFFLINE_ROOT/target"', text)
        self.assertIn('CARGO_BIN="$OFFLINE_ROOT/toolchain/bin/cargo"', text)
        self.assertIn("env -i", text)
        self.assertIn('cd /\n        env -i', text)
        self.assertIn('--manifest-path "$SOURCE_ROOT/Cargo.toml"', text)
        self.assertIn("refusing ambient root-level Cargo configuration", text)
        self.assertIn("refusing legacy Cargo-home configuration", text)
        self.assertIn("CARGO_NET_OFFLINE=true", text)
        self.assertIn("CARGO_INCREMENTAL=0", text)
        for inherited in (
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTDOCFLAGS",
            "CARGO_ENCODED_RUSTDOCFLAGS",
            "RUSTUP_TOOLCHAIN",
            "RUSTC_BOOTSTRAP",
            "CARGO_BUILD_RUSTC",
            "CARGO_BUILD_RUSTDOC",
            "CARGO_BUILD_RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        ):
            self.assertIn(inherited, text)
        self.assertGreaterEqual(text.count("--frozen"), 4)
        self.assertIn("--all-features --bins", text)
        self.assertNotIn("shell)", text)

    def test_bootstrap_cargo_environment_rejects_ambient_configuration(self) -> None:
        archive = self._build()
        parent = self.root / "ambient-parent"
        cargo_config = parent / ".cargo/config.toml"
        cargo_config.parent.mkdir(parents=True)
        cargo_config.write_text(
            "[build]\ntarget = 'attacker-target'\n", encoding="utf-8"
        )
        kit_root = self._extract_generated_zip(archive, parent / "unpacked")
        environment = dict(os.environ)
        environment.update(
            {
                "CARGO_BUILD_TARGET": "attacker-target",
                "CARGO_PROFILE_RELEASE_OPT_LEVEL": "0",
                "CARGO_SOURCE_CRATES_IO_REPLACE_WITH": "attacker",
                "RUSTFLAGS": "--cfg attacker",
            }
        )
        completed = subprocess.run(
            [str(kit_root / offline.BOOTSTRAP_NAME), "check"],
            cwd=parent,
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        log = (kit_root / ".offline/cargo-home/fixture-invocations.log").read_text(
            encoding="utf-8"
        )
        resolved_kit = kit_root.resolve()
        self.assertGreaterEqual(log.splitlines().count("/"), 4)
        self.assertIn(f"CARGO_HOME={resolved_kit / '.offline/cargo-home'}", log)
        self.assertIn(
            f"RUSTC={resolved_kit / '.offline/toolchain/bin/rustc'}", log
        )
        for forbidden in (
            "CARGO_BUILD_TARGET",
            "CARGO_PROFILE_RELEASE_OPT_LEVEL",
            "CARGO_SOURCE_CRATES_IO_REPLACE_WITH",
            "RUSTFLAGS",
            "attacker-target",
        ):
            self.assertNotIn(forbidden, log)


if __name__ == "__main__":
    unittest.main()
