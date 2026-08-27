"""Network-free tests for the opt-in foreign-kernel QEMU lab."""

from __future__ import annotations

import copy
from dataclasses import replace
import gzip
import hashlib
import importlib.util
import io
import json
import lzma
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import textwrap
import time
import tomllib
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "foreign_kernel_lab.py"
SPEC = importlib.util.spec_from_file_location("foreign_kernel_lab", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
LAB = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = LAB
SPEC.loader.exec_module(LAB)
MANIFEST_PATH = ROOT / "evidence" / "foreign_kernel_lab.toml"


class FakeResponse(io.BytesIO):
    def __init__(self, payload: bytes, url: str) -> None:
        super().__init__(payload)
        self._url = url
        self.headers = {"Content-Length": str(len(payload))}

    def geturl(self) -> str:
        return self._url

    def __enter__(self) -> "FakeResponse":
        return self

    def __exit__(self, *_arguments: object) -> None:
        self.close()


class ForeignKernelLabTests(unittest.TestCase):
    maxDiff = None

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    @staticmethod
    def identity(payload: bytes) -> object:
        return LAB.FileIdentity(len(payload), hashlib.sha256(payload).hexdigest())

    def load_raw_manifest(self) -> dict[str, object]:
        with MANIFEST_PATH.open("rb") as stream:
            return tomllib.load(stream)

    def test_committed_manifest_has_all_pinned_foreign_kernel_profiles(self) -> None:
        manifest = LAB.load_manifest(MANIFEST_PATH)

        self.assertEqual(manifest.schema, LAB.MANIFEST_SCHEMA)
        self.assertEqual(manifest.claim_class, LAB.CLAIM_CLASS)
        self.assertEqual(
            {guest.family for guest in manifest.guests},
            {"linux", "freebsd", "openbsd", "plan9", "guix", "redox"},
        )
        self.assertEqual(len(manifest.guests), 7)
        self.assertEqual(
            {guest.qemu_profile for guest in manifest.guests},
            {"aarch64-virt", "x86_64-q35"},
        )
        self.assertTrue(all("accel=tcg" in " ".join(guest.qemu_args) for guest in manifest.guests))
        self.assertTrue(all(("-nic", "none") in set(zip(guest.qemu_args, guest.qemu_args[1:])) for guest in manifest.guests))
        guix = next(guest for guest in manifest.guests if guest.family == "guix")
        self.assertIn("Welcome, this is GNU's early boot Guile.", guix.required_markers)
        self.assertTrue(any("not a claim that the kernel itself" in item for item in guix.nonclaims))
        plan9 = next(guest for guest in manifest.guests if guest.family == "plan9")
        self.assertEqual([action.trigger for action in plan9.console_actions], [
            "bootargs is (tcp, tls, il, local!device)[local!/dev/sdF0/fs]",
            "user[glenda]:",
        ])
        alpine_x86 = next(
            guest
            for guest in manifest.guests
            if guest.id == "linux-alpine-3.24.1-x86_64"
        )
        self.assertEqual(alpine_x86.architecture, "x86_64")
        self.assertEqual(
            {
                artifact.id: (artifact.size_bytes, artifact.sha256)
                for artifact in alpine_x86.artifacts
            },
            {
                "kernel": (
                    12575744,
                    "1e6bf9027720c75c3ed0d79171f21b5791ee40ca9795d07c7c6e04dc5ea2ae90",
                ),
                "initramfs": (
                    9637032,
                    "6d80a739fedeeb6cd63e24dd208845e22199c41a5fb2054941ef61ec30264fa9",
                ),
            },
        )
        openbsd = next(guest for guest in manifest.guests if guest.family == "openbsd")
        self.assertEqual(openbsd.id, "openbsd-7.9-amd64")
        self.assertEqual(openbsd.architecture, "x86_64")
        self.assertIn("{firmware:x86_64_uefi}", openbsd.qemu_args)
        self.assertEqual(
            [(action.trigger, action.occurrence) for action in openbsd.console_actions],
            [("boot>", 1), ("boot>", 2)],
        )
        self.assertEqual(
            (openbsd.artifacts[0].size_bytes, openbsd.artifacts[0].sha256),
            (
                798625792,
                "7a4a92e953618035097c796a90b54424a0f3ae775552e1e7d102cf8a5130449f",
            ),
        )

    def test_manifest_rejects_unknown_fields_and_networked_qemu(self) -> None:
        raw = self.load_raw_manifest()
        identity = LAB.hash_file(MANIFEST_PATH)
        unknown = copy.deepcopy(raw)
        unknown["unexpected"] = True
        with self.assertRaisesRegex(LAB.LabError, "unknown fields"):
            LAB.parse_manifest_data(unknown, MANIFEST_PATH, identity)

        networked = copy.deepcopy(raw)
        arguments = networked["guests"][0]["qemu_args"]
        nic_index = arguments.index("-nic")
        arguments[nic_index + 1] = "user"
        with self.assertRaisesRegex(LAB.LabError, "safe headless QEMU"):
            LAB.parse_manifest_data(networked, MANIFEST_PATH, identity)

        for suffix in (
            ["-nic", "user"],
            ["-accel", "kvm"],
            ["-M", "virt,accel=kvm"],
            ["-machine", "virt,accel=kvm"],
            ["-drive", "if=none,id=unbound"],
            ["-device", "vfio-pci,host=0000:01:00.0"],
            ["-device", "usb-host,hostbus=1,hostaddr=2"],
            ["-device", "igb"],
            ["-m", "1T"],
            ["-smp", "128"],
        ):
            with self.subTest(suffix=suffix):
                unsafe = copy.deepcopy(raw)
                unsafe["guests"][0]["qemu_args"].extend(suffix)
                with self.assertRaises(LAB.LabError):
                    LAB.parse_manifest_data(unsafe, MANIFEST_PATH, identity)

        for option, value in (
            ("-machine", "virt,gic-version=3,accel=tcg,dumpdtb=/tmp/owned"),
            ("-cpu", "max"),
            ("-smp", "128"),
            ("-m", "1T"),
            ("-kernel", "{firmware:aarch64_uefi}"),
        ):
            with self.subTest(option=option, value=value):
                unsafe = copy.deepcopy(raw)
                unsafe_arguments = unsafe["guests"][0]["qemu_args"]
                unsafe_arguments[unsafe_arguments.index(option) + 1] = value
                with self.assertRaises(LAB.LabError):
                    LAB.parse_manifest_data(unsafe, MANIFEST_PATH, identity)

        unsafe_drive = copy.deepcopy(raw)
        freebsd_raw = next(
            guest
            for guest in unsafe_drive["guests"]
            if guest["id"] == "freebsd-15.1-release-aarch64"
        )
        freebsd_arguments = freebsd_raw["qemu_args"]
        drive_index = freebsd_arguments.index("-drive")
        freebsd_arguments[drive_index + 1] += ",cache=unsafe"
        with self.assertRaises(LAB.LabError):
            LAB.parse_manifest_data(unsafe_drive, MANIFEST_PATH, identity)

        reversed_actions = copy.deepcopy(raw)
        plan9_raw = next(
            guest
            for guest in reversed_actions["guests"]
            if guest["id"] == "plan9-9front-11983-amd64"
        )
        plan9_raw["console_actions"].reverse()
        with self.assertRaisesRegex(LAB.LabError, "required_markers order"):
            LAB.parse_manifest_data(reversed_actions, MANIFEST_PATH, identity)

        invalid_occurrence = copy.deepcopy(raw)
        openbsd_raw = next(
            guest
            for guest in invalid_occurrence["guests"]
            if guest["id"] == "openbsd-7.9-amd64"
        )
        openbsd_raw["console_actions"][0]["occurrence"] = 0
        with self.assertRaisesRegex(LAB.LabError, "occurrence"):
            LAB.parse_manifest_data(invalid_occurrence, MANIFEST_PATH, identity)

        reversed_occurrences = copy.deepcopy(raw)
        openbsd_raw = next(
            guest
            for guest in reversed_occurrences["guests"]
            if guest["id"] == "openbsd-7.9-amd64"
        )
        openbsd_raw["console_actions"].reverse()
        with self.assertRaisesRegex(LAB.LabError, "strictly increasing"):
            LAB.parse_manifest_data(reversed_occurrences, MANIFEST_PATH, identity)

        control_action = copy.deepcopy(raw)
        control_action["guests"][0]["console_actions"][0]["commands"][0] = (
            "probe\u0003"
        )
        with self.assertRaisesRegex(LAB.LabError, "printable ASCII"):
            LAB.parse_manifest_data(control_action, MANIFEST_PATH, identity)

    def test_verify_file_rejects_digest_mismatch_and_symlink(self) -> None:
        path = self.root / "artifact"
        path.write_bytes(b"actual")
        with self.assertRaisesRegex(LAB.LabError, "identity mismatch"):
            LAB.verify_file(path, self.identity(b"expected"))

        link = self.root / "link"
        link.symlink_to(path)
        with self.assertRaisesRegex(LAB.LabError, "non-symlink regular file"):
            LAB.hash_file(link)

    def test_xz_fetch_is_bounded_verified_and_idempotent(self) -> None:
        expanded = b"foreign-kernel-media" * 4096
        compressed = lzma.compress(expanded)
        artifact = LAB.Artifact(
            id="media_xz",
            filename="media.img.xz",
            url="https://example.invalid/media.img.xz",
            size_bytes=len(compressed),
            sha256=hashlib.sha256(compressed).hexdigest(),
            integrity="test pin",
            unpack="xz",
            expanded_id="media",
            expanded_filename="media.img",
            expanded_size_bytes=len(expanded),
            expanded_sha256=hashlib.sha256(expanded).hexdigest(),
        )
        cache = self.root / "cache"
        response = FakeResponse(compressed, artifact.url)
        with mock.patch.object(LAB.urllib.request, "urlopen", return_value=response) as urlopen:
            first = LAB.fetch_artifact(artifact, cache)
        second = LAB.fetch_artifact(artifact, cache)

        self.assertEqual([item[0] for item in first], ["media_xz", "media"])
        self.assertEqual([item[2] for item in second], [item[2] for item in first])
        urlopen.assert_called_once()
        self.assertEqual((cache / "media.img").read_bytes(), expanded)

    def test_gzip_fetch_is_bounded_verified_and_idempotent(self) -> None:
        expanded = b"plan9-media" * 4096
        compressed = gzip.compress(expanded, mtime=0)
        artifact = LAB.Artifact(
            id="media_gz",
            filename="media.img.gz",
            url="https://example.invalid/media.img.gz",
            size_bytes=len(compressed),
            sha256=hashlib.sha256(compressed).hexdigest(),
            integrity="test pin",
            unpack="gzip",
            expanded_id="media",
            expanded_filename="media.img",
            expanded_size_bytes=len(expanded),
            expanded_sha256=hashlib.sha256(expanded).hexdigest(),
        )
        cache = self.root / "gzip-cache"
        response = FakeResponse(compressed, artifact.url)
        with mock.patch.object(LAB.urllib.request, "urlopen", return_value=response):
            fetched = LAB.fetch_artifact(artifact, cache)

        self.assertEqual([item[0] for item in fetched], ["media_gz", "media"])
        self.assertEqual((cache / "media.img").read_bytes(), expanded)

    def test_zstd_cli_fetch_is_bounded_verified_and_idempotent(self) -> None:
        expanded = b"redox-livedisk" * 4096
        compressed = b"synthetic-zstd:" + expanded
        artifact = LAB.Artifact(
            id="media_zst",
            filename="media.iso.zst",
            url="https://example.invalid/media.iso.zst",
            size_bytes=len(compressed),
            sha256=hashlib.sha256(compressed).hexdigest(),
            integrity="test pin",
            unpack="zstd",
            expanded_id="media",
            expanded_filename="media.iso",
            expanded_size_bytes=len(expanded),
            expanded_sha256=hashlib.sha256(expanded).hexdigest(),
        )
        fake_zstd = self.write_executable(
            "fake-zstd",
            """\
            #!/usr/bin/env python3
            import sys
            payload = sys.stdin.buffer.read()
            prefix = b"synthetic-zstd:"
            if not payload.startswith(prefix):
                raise SystemExit(2)
            sys.stderr.buffer.write(b"diagnostic" * 6000)
            sys.stderr.buffer.flush()
            sys.stdout.buffer.write(payload[len(prefix):])
            """,
        )
        original_import = __import__

        def without_stdlib_zstd(name: str, *args: object, **kwargs: object) -> object:
            if name == "compression":
                raise ImportError("exercise the external zstd fallback")
            return original_import(name, *args, **kwargs)

        response = FakeResponse(compressed, artifact.url)
        cache = self.root / "zstd-cache"
        with (
            mock.patch.object(LAB.urllib.request, "urlopen", return_value=response),
            mock.patch.object(LAB.shutil, "which", return_value=str(fake_zstd)),
            mock.patch("builtins.__import__", side_effect=without_stdlib_zstd),
        ):
            first = LAB.fetch_artifact(artifact, cache)
        second = LAB.fetch_artifact(artifact, cache)

        self.assertEqual([item[0] for item in first], ["media_zst", "media"])
        self.assertEqual([item[2] for item in second], [item[2] for item in first])
        self.assertEqual((cache / "media.iso").read_bytes(), expanded)

    def test_zstd_cli_has_an_end_to_end_deadline(self) -> None:
        compressed_path = self.root / "input.zst"
        compressed_path.write_bytes(b"synthetic")
        fake_zstd = self.write_executable(
            "sleeping-zstd",
            """\
            #!/usr/bin/env python3
            import time
            time.sleep(30)
            """,
        )
        original_import = __import__

        def without_stdlib_zstd(name: str, *args: object, **kwargs: object) -> object:
            if name == "compression":
                raise ImportError("exercise the external zstd fallback")
            return original_import(name, *args, **kwargs)

        pinned = LAB._open_pinned_input(compressed_path)
        try:
            with LAB._open_directory_path(self.root / "deadline-cache", create=True) as cache:
                started = time.monotonic()
                with (
                    mock.patch.object(LAB.shutil, "which", return_value=str(fake_zstd)),
                    mock.patch.object(LAB, "EXTRACTION_TIMEOUT_SECONDS", 0.1),
                    mock.patch(
                        "builtins.__import__", side_effect=without_stdlib_zstd
                    ),
                ):
                    with self.assertRaisesRegex(
                        LAB.LabError, "zstd expansion timed out"
                    ):
                        LAB._expand_zstd_at(
                            pinned,
                            cache,
                            "output",
                            self.identity(b"output"),
                        )
                self.assertLess(time.monotonic() - started, 3.0)
                self.assertFalse((cache.path / "output").exists())
        finally:
            os.close(pinned.descriptor)

    def test_x86_q35_profile_is_exact_and_rejects_unsafe_expansion(self) -> None:
        safe = (
            "-machine", "q35,accel=tcg",
            "-cpu", "qemu64",
            "-smp", "1",
            "-m", "1024M",
            "-device", "ide-cd,drive=cd0,bus=ide.0",
            "-drive", "if=none,id=cd0,media=cdrom,format=raw,readonly=on,file={artifact:media}",
            "-nic", "none",
            "-display", "none",
            "-serial", "stdio",
            "-monitor", "none",
            "-no-reboot",
        )
        LAB._validate_qemu_args("test-x86", "x86_64", "x86_64-q35", safe)
        safe_uefi = safe + ("-bios", "{firmware:x86_64_uefi}")
        LAB._validate_qemu_args(
            "test-x86-uefi", "x86_64", "x86_64-q35", safe_uefi
        )

        for firmware in (
            "{firmware:aarch64_uefi}",
            "/usr/share/ovmf/OVMF.fd",
        ):
            with self.subTest(firmware=firmware):
                with self.assertRaises(LAB.LabError):
                    LAB._validate_qemu_args(
                        "test-x86-uefi",
                        "x86_64",
                        "x86_64-q35",
                        safe + ("-bios", firmware),
                    )

        for old, new in (
            ("q35,accel=tcg", "q35,accel=kvm"),
            ("qemu64", "host"),
            ("1024M", "8G"),
            ("none", "user"),
        ):
            with self.subTest(replacement=new):
                unsafe = tuple(new if value == old else value for value in safe)
                with self.assertRaises(LAB.LabError):
                    LAB._validate_qemu_args(
                        "test-x86", "x86_64", "x86_64-q35", unsafe
                    )

    def test_iso_members_are_extracted_privately_and_verified(self) -> None:
        iso_payload = b"synthetic-iso"
        member_payload = b"verified-linux-kernel"
        member = LAB.ArtifactMember(
            id="kernel",
            path="/gnu/store/test-linux/bzImage",
            filename="guix-bzImage",
            size_bytes=len(member_payload),
            sha256=hashlib.sha256(member_payload).hexdigest(),
        )
        artifact = LAB.Artifact(
            id="media",
            filename="guix.iso",
            url="https://example.invalid/guix.iso",
            size_bytes=len(iso_payload),
            sha256=hashlib.sha256(iso_payload).hexdigest(),
            integrity="test pin",
            members=(member,),
        )
        fake_xorriso = self.write_executable(
            "fake-xorriso",
            f"""\
            #!/usr/bin/env python3
            from pathlib import Path
            import sys
            Path(sys.argv[-1]).write_bytes({member_payload!r})
            """,
        )
        response = FakeResponse(iso_payload, artifact.url)
        cache = self.root / "iso-cache"
        with (
            mock.patch.object(LAB.urllib.request, "urlopen", return_value=response),
            mock.patch.object(LAB.shutil, "which", return_value=str(fake_xorriso)),
        ):
            fetched = LAB.fetch_artifact(artifact, cache)

        self.assertEqual([item[0] for item in fetched], ["media", "kernel"])
        self.assertEqual((cache / member.filename).read_bytes(), member_payload)
        self.assertFalse(any(path.name.startswith(".iso-extract") for path in cache.iterdir()))

    def test_iso_extraction_diagnostic_capture_is_hard_bounded(self) -> None:
        iso_payload = b"synthetic-iso"
        member_payload = b"member"
        member = LAB.ArtifactMember(
            id="kernel",
            path="/kernel",
            filename="kernel.bin",
            size_bytes=len(member_payload),
            sha256=hashlib.sha256(member_payload).hexdigest(),
        )
        artifact = LAB.Artifact(
            id="media",
            filename="media.iso",
            url="https://example.invalid/media.iso",
            size_bytes=len(iso_payload),
            sha256=hashlib.sha256(iso_payload).hexdigest(),
            integrity="test pin",
            members=(member,),
        )
        fake_xorriso = self.write_executable(
            "noisy-xorriso",
            """\
            #!/usr/bin/env python3
            import os
            os.write(1, b"x" * 8192)
            """,
        )
        cache = self.root / "noisy-iso-cache"
        with (
            mock.patch.object(
                LAB.urllib.request,
                "urlopen",
                return_value=FakeResponse(iso_payload, artifact.url),
            ),
            mock.patch.object(LAB.shutil, "which", return_value=str(fake_xorriso)),
            mock.patch.object(LAB, "TOOL_DIAGNOSTIC_BYTES", 1024),
        ):
            with self.assertRaisesRegex(LAB.LabError, "stdout exceeded 1024"):
                LAB.fetch_artifact(artifact, cache)

        self.assertFalse((cache / member.filename).exists())
        self.assertFalse(
            any(path.name.startswith(".iso-extract") for path in cache.iterdir())
        )

    def test_guest_cache_symlink_is_rejected_without_external_write(self) -> None:
        guest = self.make_guest(timeout=1.0)
        guest_root = self.root / "guests"
        guest_root.mkdir()
        external = self.root / "external"
        external.mkdir()
        (guest_root / guest.cache_dir).symlink_to(external, target_is_directory=True)

        with mock.patch.object(LAB.urllib.request, "urlopen") as urlopen:
            with self.assertRaisesRegex(LAB.LabError, "non-symlink directory"):
                LAB.fetch_guest(guest, guest_root)

        urlopen.assert_not_called()
        self.assertEqual(list(external.iterdir()), [])

    def test_cache_directory_replacement_cannot_redirect_publication(self) -> None:
        guest = self.make_guest(timeout=1.0)
        guest_root = self.root / "guests"
        cache = guest_root / guest.cache_dir
        cache.mkdir(parents=True)
        retained = guest_root / "retained-cache"
        external = self.root / "external"
        external.mkdir()
        payload = b"kernel"
        response = FakeResponse(payload, guest.artifacts[0].url)

        def replace_cache(*_arguments: object, **_keywords: object) -> FakeResponse:
            cache.rename(retained)
            cache.symlink_to(external, target_is_directory=True)
            return response

        with mock.patch.object(
            LAB.urllib.request, "urlopen", side_effect=replace_cache
        ):
            fetched = LAB.fetch_guest(guest, guest_root)

        self.assertEqual(fetched[0][2], self.identity(payload))
        self.assertEqual((retained / "kernel.bin").read_bytes(), payload)
        self.assertEqual(list(external.iterdir()), [])
        with self.assertRaisesRegex(LAB.LabError, "non-symlink directory"):
            LAB.fetch_guest(guest, guest_root)

    def test_atomic_publish_accepts_exact_winner_and_rejects_mismatch(self) -> None:
        expected_payload = b"admitted"
        expected = self.identity(expected_payload)
        destination = self.root / "artifact"
        destination.write_bytes(expected_payload)
        temporary = self.root / "exact.part"
        temporary.write_bytes(expected_payload)

        observed = LAB._atomic_publish(temporary, destination, expected)

        self.assertEqual(observed, expected)
        self.assertFalse(temporary.exists())
        destination.write_bytes(b"wrong")
        mismatched = self.root / "mismatched.part"
        mismatched.write_bytes(expected_payload)
        with self.assertRaisesRegex(LAB.LabError, "identity mismatch"):
            LAB._atomic_publish(mismatched, destination, expected)
        self.assertFalse(mismatched.exists())

    def test_download_rejects_declared_size_before_publication(self) -> None:
        payload = b"too-large"
        artifact = LAB.Artifact(
            id="kernel",
            filename="kernel",
            url="https://example.invalid/kernel",
            size_bytes=3,
            sha256=hashlib.sha256(payload[:3]).hexdigest(),
            integrity="test pin",
        )
        response = FakeResponse(payload, artifact.url)
        with mock.patch.object(LAB.urllib.request, "urlopen", return_value=response):
            with self.assertRaisesRegex(LAB.LabError, "server length mismatch"):
                LAB.fetch_artifact(artifact, self.root)
        self.assertFalse((self.root / "kernel").exists())

    def test_terminal_normalization_and_marker_contract(self) -> None:
        guest = self.make_guest(timeout=1.0)
        raw = b"|\b\x1b[31mBOOT\x1b[0m\r\nSHELL\nREADY\nARCH\nPOWER\n"
        normalized = LAB.normalize_terminal(raw)
        validation = LAB.validate_transcript(guest, normalized)

        self.assertIn("BOOT", normalized)
        self.assertTrue(validation["complete"], validation)
        duplicate = LAB.validate_transcript(guest, normalized + "READY\n")
        self.assertIn("duplicated required markers", duplicate["issues"][0])
        forbidden = LAB.validate_transcript(guest, normalized + "PANIC\n")
        self.assertTrue(forbidden["forbidden_counts"])

    def test_git_context_rejects_a_mixed_repository_snapshot(self) -> None:
        stable = {
            "commit": b"a" * 40,
            "status": b"",
            "tracked_diff": b"",
            "untracked_paths": (),
            "untracked_sha256": hashlib.sha256().hexdigest(),
        }
        changed = {**stable, "status": b" M scripts/foreign_kernel_lab.py\0"}
        with (
            mock.patch.object(
                LAB,
                "_git_bytes",
                return_value=(str(ROOT) + "\n").encode(),
            ),
            mock.patch.object(
                LAB, "_capture_git_state", side_effect=(stable, changed)
            ),
        ):
            with self.assertRaisesRegex(
                LAB.LabError, "repository state changed while provenance was captured"
            ):
                LAB._git_context()

    def make_guest(self, *, timeout: float) -> object:
        payload = b"kernel"
        artifact = LAB.Artifact(
            id="kernel",
            filename="kernel.bin",
            url="https://example.invalid/kernel.bin",
            size_bytes=len(payload),
            sha256=hashlib.sha256(payload).hexdigest(),
            integrity="test pin",
        )
        return LAB.Guest(
            id="test-linux",
            family="linux",
            version="test",
            architecture="aarch64",
            qemu_profile="aarch64-virt",
            cache_dir="test-linux",
            qemu_executable="qemu-system-aarch64",
            timeout_seconds=timeout,
            post_completion_seconds=0.02,
            max_capture_bytes=65536,
            qemu_args=(
                "-machine", "virt,accel=tcg",
                "-nic", "none",
                "-display", "none",
                "-serial", "stdio",
                "-monitor", "none",
                "-no-reboot",
                "-kernel", "{artifact:kernel}",
            ),
            required_markers=("BOOT", "SHELL", "READY", "ARCH", "POWER"),
            unique_markers=("BOOT", "SHELL", "READY", "ARCH", "POWER"),
            forbidden_markers=("PANIC",),
            console_actions=(LAB.ConsoleAction("SHELL", ("probe",)),),
            claim="test claim",
            nonclaims=("test nonclaim",),
            artifacts=(artifact,),
        )

    def make_manifest(self, guest: object) -> object:
        manifest_path = self.root / "manifest.toml"
        manifest_path.write_text("schema = 'test'\n", encoding="utf-8")
        return LAB.Manifest(
            path=manifest_path,
            identity=LAB.hash_file(manifest_path),
            schema=LAB.MANIFEST_SCHEMA,
            claim_class=LAB.CLAIM_CLASS,
            claims=("test claim",),
            nonclaims=("test nonclaim",),
            firmware={},
            guests=(guest,),
        )

    def write_executable(self, name: str, source: str) -> Path:
        path = self.root / name
        path.write_text(textwrap.dedent(source), encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IXUSR)
        return path

    def prepare_kernel(self, guest: object) -> Path:
        guest_root = self.root / "guests"
        cache = guest_root / guest.cache_dir
        cache.mkdir(parents=True)
        (cache / "kernel.bin").write_bytes(b"kernel")
        return guest_root

    def test_fake_qemu_success_records_bounded_observation(self) -> None:
        guest = self.make_guest(timeout=2.0)
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu",
            """\
            #!/usr/bin/env python3
            import os
            import sys
            if "--version" in sys.argv:
                if "OSTADIX_TEST_SECRET" in os.environ:
                    raise SystemExit(8)
                if not os.path.samefile(os.path.dirname(os.environ["HOME"]), os.getcwd()):
                    raise SystemExit(9)
                print("QEMU emulator version test")
                raise SystemExit(0)
            print("BOOT", flush=True)
            print("SHELL", flush=True)
            if sys.stdin.readline().strip() == "probe":
                print("READY", flush=True)
                print("ARCH", flush=True)
                print("POWER", flush=True)
            """,
        )

        with mock.patch.dict(os.environ, {"OSTADIX_TEST_SECRET": "not-inherited"}):
            observation = LAB.run_guest(
                manifest,
                guest,
                guest_root,
                self.root / "output",
                qemu_override=qemu,
            )

        self.assertEqual(observation["status"], "synthetic-passed", observation)
        self.assertFalse(observation["claim_admissible"])
        self.assertFalse(observation["qemu"]["executor_claim_admitted"])
        self.assertEqual(
            observation["qemu"]["executor_origin"], "explicit-untrusted-override"
        )
        self.assertTrue(observation["runtime"]["console_commands_sent"])
        self.assertEqual(observation["qemu"]["version"], "QEMU emulator version test")
        self.assertTrue(observation["runtime"]["cleanup_resolved"])
        self.assertTrue(observation["runtime"]["drain_complete"])
        self.assertRegex(observation["repository"]["source_commit"], r"^[0-9a-f]{40,64}$")
        self.assertRegex(
            observation["repository"]["working_tree_state_sha256"],
            r"^[0-9a-f]{64}$",
        )
        self.assertRegex(
            observation["repository"]["harness"]["sha256"], r"^[0-9a-f]{64}$"
        )
        self.assertEqual(
            observation["artifacts"]["kernel"]["transport"],
            "inherited-read-only-fd",
        )
        self.assertTrue(
            observation["qemu"]["transport"].startswith("private-verified-copy")
        )
        self.assertEqual(
            observation["qemu"]["post_run_sha256"],
            observation["qemu"]["sha256"],
        )
        self.assertTrue(Path(observation["observation_path"]).is_file())

    def test_manifest_named_path_qemu_is_the_only_claim_admitted_executor(self) -> None:
        guest = self.make_guest(timeout=2.0)
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "qemu-system-aarch64",
            """\
            #!/usr/bin/env python3
            import sys
            if "--version" in sys.argv:
                print("QEMU emulator version 9.1.0")
                raise SystemExit(0)
            print("BOOT", flush=True)
            print("SHELL", flush=True)
            if sys.stdin.readline().strip() == "probe":
                print("READY", flush=True)
                print("ARCH", flush=True)
                print("POWER", flush=True)
            """,
        )

        with mock.patch.object(LAB.shutil, "which", return_value=str(qemu)):
            observation = LAB.run_guest(
                manifest,
                guest,
                guest_root,
                self.root / "path-qemu-output",
            )

        self.assertEqual(observation["status"], "passed", observation)
        self.assertTrue(observation["claim_admissible"])
        self.assertTrue(observation["qemu"]["executor_claim_admitted"])
        self.assertTrue(observation["qemu"]["version_banner_admitted"])
        self.assertEqual(
            observation["qemu"]["executor_origin"],
            "manifest-named-path-discovery",
        )

    def test_qemu_version_capture_is_hard_bounded_and_cleaned(self) -> None:
        guest = self.make_guest(timeout=2.0)
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu-noisy-version",
            """\
            #!/usr/bin/env python3
            import os
            import sys
            if "--version" in sys.argv:
                os.write(1, b"v" * 65536)
                raise SystemExit(0)
            raise SystemExit(7)
            """,
        )

        started = time.monotonic()
        with self.assertRaisesRegex(
            LAB.LabError, "QEMU version inspection stdout exceeded"
        ):
            LAB.run_guest(
                manifest,
                guest,
                guest_root,
                self.root / "noisy-version-output",
                qemu_override=qemu,
            )
        self.assertLess(time.monotonic() - started, 3.0)

    def test_prompt_phased_console_actions_wait_for_each_trigger(self) -> None:
        guest = replace(
            self.make_guest(timeout=2.0),
            required_markers=("BOOT", "SHELL", "USER", "READY", "ARCH", "POWER"),
            unique_markers=("BOOT", "SHELL", "USER", "READY", "ARCH", "POWER"),
            console_actions=(
                LAB.ConsoleAction("SHELL", ("root-device",)),
                LAB.ConsoleAction("USER", ("glenda",)),
            ),
        )
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu-phased",
            """\
            #!/usr/bin/env python3
            import sys
            if "--version" in sys.argv:
                print("QEMU emulator version test")
                raise SystemExit(0)
            print("BOOT", flush=True)
            print("SHELL", flush=True)
            if sys.stdin.readline().strip() != "root-device":
                raise SystemExit(3)
            print("USER", flush=True)
            if sys.stdin.readline().strip() != "glenda":
                raise SystemExit(4)
            print("READY", flush=True)
            print("ARCH", flush=True)
            print("POWER", flush=True)
            """,
        )

        observation = LAB.run_guest(
            manifest,
            guest,
            guest_root,
            self.root / "phased-output",
            qemu_override=qemu,
        )

        self.assertEqual(observation["status"], "synthetic-passed", observation)
        self.assertEqual(observation["runtime"]["console_actions_total"], 2)
        self.assertEqual(observation["runtime"]["console_actions_sent"], 2)

    def test_console_actions_wait_for_counted_trigger_occurrence(self) -> None:
        guest = replace(
            self.make_guest(timeout=2.0),
            required_markers=("BOOT", "SHELL", "READY", "ARCH", "POWER"),
            unique_markers=("BOOT", "READY", "ARCH", "POWER"),
            console_actions=(
                LAB.ConsoleAction("SHELL", ("set-console",), occurrence=1),
                LAB.ConsoleAction("SHELL", ("boot",), occurrence=2),
            ),
        )
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu-counted-prompt",
            """\
            #!/usr/bin/env python3
            import select
            import sys
            import time
            if "--version" in sys.argv:
                print("QEMU emulator version test")
                raise SystemExit(0)
            print("BOOT", flush=True)
            print("SHELL", flush=True)
            if sys.stdin.readline().strip() != "set-console":
                raise SystemExit(3)
            time.sleep(0.2)
            if select.select([sys.stdin], [], [], 0)[0]:
                raise SystemExit(4)
            print("SHELL", flush=True)
            if sys.stdin.readline().strip() != "boot":
                raise SystemExit(5)
            print("READY", flush=True)
            print("ARCH", flush=True)
            print("POWER", flush=True)
            """,
        )

        observation = LAB.run_guest(
            manifest,
            guest,
            guest_root,
            self.root / "counted-prompt-output",
            qemu_override=qemu,
        )

        self.assertEqual(observation["status"], "synthetic-passed", observation)
        self.assertEqual(observation["runtime"]["console_actions_total"], 2)
        self.assertEqual(observation["runtime"]["console_actions_sent"], 2)

    def test_path_swap_after_verification_cannot_change_launched_input(self) -> None:
        guest = self.make_guest(timeout=2.0)
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        kernel_path = guest_root / guest.cache_dir / "kernel.bin"
        qemu = self.write_executable(
            "fake-qemu-fd",
            """\
            #!/usr/bin/env python3
            from pathlib import Path
            import sys
            if "--version" in sys.argv:
                print("QEMU emulator version test")
                raise SystemExit(0)
            kernel = Path(sys.argv[sys.argv.index("-kernel") + 1]).read_bytes()
            if kernel != b"kernel":
                print("PANIC", flush=True)
                raise SystemExit(7)
            print("BOOT", flush=True)
            print("SHELL", flush=True)
            if sys.stdin.readline().strip() == "probe":
                print("READY", flush=True)
                print("ARCH", flush=True)
                print("POWER", flush=True)
            """,
        )
        real_popen = subprocess.Popen
        swapped = False

        def swap_then_launch(*arguments: object, **keywords: object) -> object:
            nonlocal swapped
            command = arguments[0] if arguments else keywords.get("args")
            if (
                not swapped
                and isinstance(command, (list, tuple))
                and command
                and "-kernel" in command
            ):
                replacement = kernel_path.with_suffix(".replacement")
                replacement.write_bytes(b"malicious replacement")
                os.replace(replacement, kernel_path)
                swapped = True
            return real_popen(*arguments, **keywords)

        with mock.patch.object(LAB.subprocess, "Popen", side_effect=swap_then_launch):
            observation = LAB.run_guest(
                manifest,
                guest,
                guest_root,
                self.root / "fd-output",
                qemu_override=qemu,
            )

        self.assertTrue(swapped)
        self.assertEqual(kernel_path.read_bytes(), b"malicious replacement")
        self.assertEqual(observation["status"], "synthetic-passed", observation)
        self.assertEqual(observation["runtime"]["input_stability_issues"], [])

    def test_qemu_path_swap_cannot_change_pinned_executable(self) -> None:
        guest = self.make_guest(timeout=2.0)
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu-original",
            """\
            #!/usr/bin/env python3
            import sys
            if "--version" in sys.argv:
                print("QEMU emulator version original")
                raise SystemExit(0)
            print("BOOT", flush=True)
            print("SHELL", flush=True)
            if sys.stdin.readline().strip() == "probe":
                print("READY", flush=True)
                print("ARCH", flush=True)
                print("POWER", flush=True)
            """,
        )
        original_identity = LAB.hash_file(qemu)
        replacement = self.write_executable(
            "fake-qemu-replacement",
            """\
            #!/usr/bin/env python3
            import sys
            if "--version" in sys.argv:
                print("QEMU emulator version replacement")
                raise SystemExit(0)
            print("PANIC", flush=True)
            raise SystemExit(7)
            """,
        )
        real_popen = subprocess.Popen
        swapped = False

        def swap_then_launch(*arguments: object, **keywords: object) -> object:
            nonlocal swapped
            command = arguments[0] if arguments else keywords.get("args")
            if (
                not swapped
                and isinstance(command, (list, tuple))
                and "-kernel" in command
            ):
                os.replace(replacement, qemu)
                swapped = True
            return real_popen(*arguments, **keywords)

        with mock.patch.object(LAB.subprocess, "Popen", side_effect=swap_then_launch):
            observation = LAB.run_guest(
                manifest,
                guest,
                guest_root,
                self.root / "qemu-fd-output",
                qemu_override=qemu,
            )

        self.assertTrue(swapped)
        self.assertEqual(observation["status"], "synthetic-passed", observation)
        self.assertEqual(observation["qemu"]["version"], "QEMU emulator version original")
        self.assertEqual(observation["qemu"]["sha256"], original_identity.sha256)
        self.assertEqual(observation["qemu"]["post_run_sha256"], original_identity.sha256)
        self.assertNotEqual(LAB.hash_file(qemu), original_identity)

    def test_queued_pre_cleanup_stderr_fails_closed(self) -> None:
        guest = self.make_guest(timeout=2.0)
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu-queued-stderr",
            """\
            #!/usr/bin/env python3
            import sys
            import time
            if "--version" in sys.argv:
                print("QEMU emulator version test")
                raise SystemExit(0)
            print("BOOT", flush=True)
            print("SHELL", flush=True)
            if sys.stdin.readline().strip() == "probe":
                print("READY", flush=True)
                print("ARCH", flush=True)
                print("POWER", flush=True)
                print("fatal queued before cleanup", file=sys.stderr, flush=True)
                time.sleep(5)
            """,
        )
        real_selector = LAB.selectors.DefaultSelector
        selector_calls = 0

        class StdoutOnlySelector:
            def __init__(self) -> None:
                self.delegate = real_selector()

            def register(self, *arguments: object, **keywords: object) -> object:
                return self.delegate.register(*arguments, **keywords)

            def unregister(self, *arguments: object, **keywords: object) -> object:
                return self.delegate.unregister(*arguments, **keywords)

            def get_map(self) -> object:
                return self.delegate.get_map()

            def select(self, *arguments: object, **keywords: object) -> list[object]:
                return [
                    event
                    for event in self.delegate.select(*arguments, **keywords)
                    if event[0].data != "stderr"
                ]

            def close(self) -> None:
                self.delegate.close()

        def hide_boot_stderr() -> object:
            nonlocal selector_calls
            selector_calls += 1
            if selector_calls == 1:
                return real_selector()
            return StdoutOnlySelector()

        with mock.patch.object(
            LAB.selectors, "DefaultSelector", side_effect=hide_boot_stderr
        ):
            observation = LAB.run_guest(
                manifest,
                guest,
                guest_root,
                self.root / "queued-stderr-output",
                qemu_override=qemu,
            )

        self.assertEqual(observation["status"], "failed", observation)
        self.assertGreater(observation["runtime"]["pre_cleanup_stderr_size"], 0)
        self.assertRegex(
            observation["runtime"]["pre_cleanup_stderr_sha256"], r"^[0-9a-f]{64}$"
        )

    def test_selector_setup_failure_cleans_launched_process_group(self) -> None:
        guest = self.make_guest(timeout=2.0)
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu-selector-failure",
            """\
            #!/usr/bin/env python3
            import sys
            import time
            if "--version" in sys.argv:
                print("QEMU emulator version test")
                raise SystemExit(0)
            time.sleep(30)
            """,
        )
        real_popen = subprocess.Popen
        real_selector = LAB.selectors.DefaultSelector
        launched: list[object] = []
        selector_calls = 0

        def capture_launch(*arguments: object, **keywords: object) -> object:
            command = arguments[0] if arguments else keywords.get("args")
            process = real_popen(*arguments, **keywords)
            if isinstance(command, (list, tuple)) and "-kernel" in command:
                launched.append(process)
            return process

        def fail_boot_selector() -> object:
            nonlocal selector_calls
            selector_calls += 1
            if selector_calls == 1:
                return real_selector()
            raise OSError("selector construction failed")

        with mock.patch.object(LAB.subprocess, "Popen", side_effect=capture_launch):
            with mock.patch.object(
                LAB.selectors,
                "DefaultSelector",
                side_effect=fail_boot_selector,
            ):
                with self.assertRaisesRegex(OSError, "selector construction failed"):
                    LAB.run_guest(
                        manifest,
                        guest,
                        guest_root,
                        self.root / "selector-failure-output",
                        qemu_override=qemu,
                    )

        self.assertEqual(len(launched), 1)
        process = launched[0]
        self.assertIsNotNone(process.poll())
        self.assertFalse(LAB._process_group_exists(process.pid))

    def test_selector_close_failure_cannot_preempt_process_cleanup(self) -> None:
        guest = self.make_guest(timeout=2.0)
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu-selector-close-failure",
            """\
            #!/usr/bin/env python3
            import sys
            import time
            if "--version" in sys.argv:
                print("QEMU emulator version test")
                raise SystemExit(0)
            print("BOOT", flush=True)
            print("SHELL", flush=True)
            if sys.stdin.readline().strip() == "probe":
                print("READY", flush=True)
                print("ARCH", flush=True)
                print("POWER", flush=True)
                time.sleep(30)
            """,
        )
        real_selector = LAB.selectors.DefaultSelector
        real_popen = subprocess.Popen
        launched: list[object] = []
        selector_calls = 0

        class CloseFailureSelector:
            def __init__(self) -> None:
                self.delegate = real_selector()

            def register(self, *arguments: object, **keywords: object) -> object:
                return self.delegate.register(*arguments, **keywords)

            def unregister(self, *arguments: object, **keywords: object) -> object:
                return self.delegate.unregister(*arguments, **keywords)

            def get_map(self) -> object:
                return self.delegate.get_map()

            def select(self, *arguments: object, **keywords: object) -> object:
                return self.delegate.select(*arguments, **keywords)

            def close(self) -> None:
                self.delegate.close()
                raise OSError("selector close failed")

        def capture_launch(*arguments: object, **keywords: object) -> object:
            command = arguments[0] if arguments else keywords.get("args")
            process = real_popen(*arguments, **keywords)
            if isinstance(command, (list, tuple)) and "-kernel" in command:
                launched.append(process)
            return process

        def fail_boot_selector_close() -> object:
            nonlocal selector_calls
            selector_calls += 1
            if selector_calls == 1:
                return real_selector()
            return CloseFailureSelector()

        with mock.patch.object(LAB.subprocess, "Popen", side_effect=capture_launch):
            with mock.patch.object(
                LAB.selectors,
                "DefaultSelector",
                side_effect=fail_boot_selector_close,
            ):
                with self.assertRaisesRegex(OSError, "selector close failed"):
                    LAB.run_guest(
                        manifest,
                        guest,
                        guest_root,
                        self.root / "selector-close-output",
                        qemu_override=qemu,
                    )

        self.assertEqual(len(launched), 1)
        process = launched[0]
        self.assertIsNotNone(process.poll())
        self.assertFalse(LAB._process_group_exists(process.pid))

    def test_extracted_source_release_uses_verified_manifest_provenance(self) -> None:
        release_root = self.root / "Ostadix-lang-source-test"
        script_path = release_root / "scripts" / "foreign_kernel_lab.py"
        evidence_path = release_root / "evidence" / "test-manifest.toml"
        script_path.parent.mkdir(parents=True)
        evidence_path.parent.mkdir(parents=True)
        script_path.write_bytes(MODULE_PATH.read_bytes())
        script_path.chmod(0o755)
        evidence_path.write_text("schema = 'test'\n", encoding="utf-8")
        entries = []
        for relative, path, mode in (
            ("evidence/test-manifest.toml", evidence_path, "100644"),
            ("scripts/foreign_kernel_lab.py", script_path, "100755"),
        ):
            payload = path.read_bytes()
            entries.append(
                {
                    "mode": mode,
                    "path": relative,
                    "sha256": hashlib.sha256(payload).hexdigest(),
                    "size": len(payload),
                }
            )
        commit = "a" * 40
        source_manifest = {
            "commit": commit,
            "file_count": len(entries),
            "files": entries,
            "prefix": release_root.name,
            "schema": LAB.SOURCE_RELEASE_SCHEMA,
        }
        source_manifest_bytes = (
            json.dumps(
                source_manifest,
                ensure_ascii=True,
                sort_keys=True,
                separators=(",", ":"),
            )
            + "\n"
        ).encode("ascii")
        (release_root / LAB.SOURCE_RELEASE_MANIFEST).write_bytes(
            source_manifest_bytes
        )
        checksum_lines = [
            f"{entry['sha256']}  {entry['path']}" for entry in entries
        ]
        checksum_lines.append(
            f"{hashlib.sha256(source_manifest_bytes).hexdigest()}  "
            f"{LAB.SOURCE_RELEASE_MANIFEST}"
        )
        (release_root / LAB.SOURCE_RELEASE_CHECKSUMS).write_text(
            "\n".join(checksum_lines) + "\n", encoding="utf-8"
        )

        module_name = f"foreign_kernel_release_{os.getpid()}_{id(self)}"
        spec = importlib.util.spec_from_file_location(module_name, script_path)
        assert spec is not None and spec.loader is not None
        release_lab = importlib.util.module_from_spec(spec)
        sys.modules[module_name] = release_lab
        try:
            spec.loader.exec_module(release_lab)
            artifact_payload = b"kernel"
            artifact = release_lab.Artifact(
                id="kernel",
                filename="kernel.bin",
                url="https://example.invalid/kernel.bin",
                size_bytes=len(artifact_payload),
                sha256=hashlib.sha256(artifact_payload).hexdigest(),
                integrity="test pin",
            )
            guest = release_lab.Guest(
                id="release-linux",
                family="linux",
                version="test",
                architecture="aarch64",
                qemu_profile="aarch64-virt",
                cache_dir="release-linux",
                qemu_executable="qemu-system-aarch64",
                timeout_seconds=2.0,
                post_completion_seconds=0.02,
                max_capture_bytes=65536,
                qemu_args=(
                    "-machine", "virt,accel=tcg",
                    "-nic", "none",
                    "-display", "none",
                    "-serial", "stdio",
                    "-monitor", "none",
                    "-no-reboot",
                    "-kernel", "{artifact:kernel}",
                ),
                required_markers=("BOOT", "SHELL", "READY", "ARCH", "POWER"),
                unique_markers=("BOOT", "SHELL", "READY", "ARCH", "POWER"),
                forbidden_markers=("PANIC",),
                console_actions=(
                    release_lab.ConsoleAction("SHELL", ("probe",)),
                ),
                claim="test claim",
                nonclaims=("test nonclaim",),
                artifacts=(artifact,),
            )
            manifest = release_lab.Manifest(
                path=evidence_path,
                identity=release_lab.hash_file(evidence_path),
                schema=release_lab.MANIFEST_SCHEMA,
                claim_class=release_lab.CLAIM_CLASS,
                claims=("test claim",),
                nonclaims=("test nonclaim",),
                firmware={},
                guests=(guest,),
            )
            guest_root = self.root / "release-guests"
            cache = guest_root / guest.cache_dir
            cache.mkdir(parents=True)
            (cache / "kernel.bin").write_bytes(artifact_payload)
            qemu = self.write_executable(
                "fake-qemu-source-release",
                """\
                #!/usr/bin/env python3
                import sys
                if "--version" in sys.argv:
                    print("QEMU emulator version release")
                    raise SystemExit(0)
                print("BOOT", flush=True)
                print("SHELL", flush=True)
                if sys.stdin.readline().strip() == "probe":
                    print("READY", flush=True)
                    print("ARCH", flush=True)
                    print("POWER", flush=True)
                """,
            )

            observation = release_lab.run_guest(
                manifest,
                guest,
                guest_root,
                self.root / "release-output",
                qemu_override=qemu,
            )
        finally:
            sys.modules.pop(module_name, None)

        self.assertEqual(observation["status"], "synthetic-passed", observation)
        self.assertEqual(
            observation["repository"]["provenance_kind"],
            "source-release-manifest",
        )
        self.assertEqual(observation["repository"]["source_commit"], commit)
        self.assertFalse(observation["repository"]["untracked_files_audited"])

    def test_nonzero_qemu_with_stderr_fails_and_binds_stderr(self) -> None:
        guest = self.make_guest(timeout=2.0)
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu-error",
            """\
            #!/usr/bin/env python3
            import sys
            if "--version" in sys.argv:
                print("QEMU emulator version test")
                raise SystemExit(0)
            print("BOOT", flush=True)
            print("SHELL", flush=True)
            if sys.stdin.readline().strip() == "probe":
                print("READY", flush=True)
                print("ARCH", flush=True)
                print("POWER", flush=True)
                print("fatal qemu failure", file=sys.stderr, flush=True)
                raise SystemExit(9)
            """,
        )

        observation = LAB.run_guest(
            manifest,
            guest,
            guest_root,
            self.root / "error-output",
            qemu_override=qemu,
        )

        self.assertEqual(observation["status"], "failed", observation)
        self.assertEqual(observation["runtime"]["pre_cleanup_returncode"], 9)
        self.assertFalse(observation["runtime"]["exit_admissible"])
        self.assertGreater(observation["transcript"]["stderr_size_bytes"], 0)
        self.assertRegex(observation["transcript"]["stderr_sha256"], r"^[0-9a-f]{64}$")

    def test_fake_qemu_timeout_is_failed_and_cleaned_up(self) -> None:
        guest = self.make_guest(timeout=0.15)
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu-timeout",
            """\
            #!/usr/bin/env python3
            import sys
            import time
            if "--version" in sys.argv:
                print("QEMU emulator version test")
                raise SystemExit(0)
            print("BOOT", flush=True)
            time.sleep(10)
            """,
        )

        observation = LAB.run_guest(
            manifest,
            guest,
            guest_root,
            self.root / "timeout-output",
            qemu_override=qemu,
        )

        self.assertEqual(observation["status"], "failed")
        self.assertTrue(observation["runtime"]["timed_out"])
        self.assertIn(observation["runtime"]["cleanup_action"], {"terminate", "kill"})

    def test_continuous_output_overflow_reaches_process_cleanup(self) -> None:
        guest = replace(
            self.make_guest(timeout=2.0),
            max_capture_bytes=4096,
            console_actions=(),
        )
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu-output-flood",
            """\
            #!/usr/bin/env python3
            import os
            import sys
            if "--version" in sys.argv:
                print("QEMU emulator version test")
                raise SystemExit(0)
            while True:
                os.write(1, b"x" * 65536)
            """,
        )

        started = time.monotonic()
        observation = LAB.run_guest(
            manifest,
            guest,
            guest_root,
            self.root / "flood-output",
            qemu_override=qemu,
        )

        self.assertLess(time.monotonic() - started, 3.0)
        self.assertEqual(observation["status"], "failed", observation)
        self.assertTrue(observation["runtime"]["capture_overflow"])
        self.assertTrue(observation["runtime"]["cleanup_resolved"], observation)

    def test_cleanup_terminates_live_leader_and_orphaned_group_child(self) -> None:
        sleeper = subprocess.Popen(["/bin/sleep", "30"], start_new_session=True)
        action, _returncode = LAB._cleanup_process(sleeper, timeout_seconds=0.5)
        self.assertIn(action, {"terminate", "kill"})
        self.assertFalse(LAB._process_group_exists(sleeper.pid))

        guest = self.make_guest(timeout=2.0)
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu-child",
            """\
            #!/usr/bin/env python3
            import os
            import signal
            import sys
            import time
            if "--version" in sys.argv:
                print("QEMU emulator version test")
                raise SystemExit(0)
            child = os.fork()
            if child == 0:
                signal.signal(signal.SIGTERM, lambda *_: raise_exit())
                def raise_exit():
                    raise SystemExit(0)
                time.sleep(30)
                raise SystemExit(0)
            print(f"CHILD_PID={child}", flush=True)
            print("BOOT", flush=True)
            print("SHELL", flush=True)
            if sys.stdin.readline().strip() == "probe":
                print("READY", flush=True)
                print("ARCH", flush=True)
                print("POWER", flush=True)
            raise SystemExit(0)
            """,
        )

        started = time.monotonic()
        observation = LAB.run_guest(
            manifest,
            guest,
            guest_root,
            self.root / "child-output",
            qemu_override=qemu,
        )
        elapsed = time.monotonic() - started

        self.assertLess(elapsed, 3.0)
        self.assertTrue(observation["runtime"]["cleanup_resolved"], observation)
        self.assertTrue(observation["runtime"]["drain_complete"], observation)
        self.assertIn(observation["runtime"]["cleanup_action"], {"terminate", "kill"})

    def test_escaped_pipe_holder_fails_without_blocking_harness(self) -> None:
        guest = self.make_guest(timeout=2.0)
        manifest = self.make_manifest(guest)
        guest_root = self.prepare_kernel(guest)
        qemu = self.write_executable(
            "fake-qemu-escaped-child",
            """\
            #!/usr/bin/env python3
            import os
            import sys
            import time
            if "--version" in sys.argv:
                print("QEMU emulator version test")
                raise SystemExit(0)
            child = os.fork()
            if child == 0:
                os.setsid()
                print(f"ESCAPED_PID={os.getpid()}", flush=True)
                time.sleep(5)
                raise SystemExit(0)
            print("BOOT", flush=True)
            print("SHELL", flush=True)
            if sys.stdin.readline().strip() == "probe":
                print("READY", flush=True)
                print("ARCH", flush=True)
                print("POWER", flush=True)
            raise SystemExit(0)
            """,
        )

        started = time.monotonic()
        observation = LAB.run_guest(
            manifest,
            guest,
            guest_root,
            self.root / "escaped-output",
            qemu_override=qemu,
        )
        elapsed = time.monotonic() - started
        transcript = Path(observation["transcript"]["normalized_path"]).read_text()
        escaped_line = next(
            line for line in transcript.splitlines() if line.startswith("ESCAPED_PID=")
        )
        escaped_pid = int(escaped_line.split("=", 1)[1])
        try:
            self.assertLess(elapsed, 3.0)
            self.assertEqual(observation["status"], "failed", observation)
            self.assertTrue(observation["runtime"]["cleanup_resolved"], observation)
            self.assertFalse(observation["runtime"]["drain_complete"], observation)
        finally:
            try:
                os.kill(escaped_pid, 9)
            except ProcessLookupError:
                pass


if __name__ == "__main__":
    unittest.main()
