#!/usr/bin/env python3

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import time
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]


def _load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


RELEASE = _load(
    "ostadix_hosted_live_release",
    ROOT / "scripts/ostadix_hosted_live_release.py",
)
SMOKE = _load(
    "ostadix_hosted_live_qemu_smoke",
    ROOT / "ocore/kernel/smoke-x86_64-hosted-live-qemu.py",
)
VGA_SMOKE = _load(
    "ostadix_hosted_live_qemu_vga_smoke",
    ROOT / "ocore/kernel/smoke-x86_64-hosted-live-vga-qemu.py",
)


class HostedLiveReleaseTests(unittest.TestCase):
    def git(self, root: Path, *arguments: str) -> str:
        result = subprocess.run(
            ["git", "-C", str(root), *arguments],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def repository(self, root: Path) -> None:
        self.git(root, "init", "-q")
        self.git(root, "config", "user.email", "fixture@example.invalid")
        self.git(root, "config", "user.name", "Fixture")
        for relative in RELEASE.REQUIRED_ARCHIVE_PATHS:
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"fixture {relative}\n", encoding="utf-8")
        tracked = root / "tracked.txt"
        tracked.write_text("committed\n", encoding="utf-8")
        self.git(root, "add", "--all")
        self.git(root, "commit", "-q", "-m", "fixture")

    def complete_receipt(
        self,
        snapshot,
        output: Path,
        receipt: Path,
    ):
        identity = {"bytes": 7, "sha256": "4" * 64}
        inspection = {
            "schema": "ostadix.capacity-iso/v1",
            "architecture": "x86_64",
            "volume_id": "OSTADIX_CAPACITY",
            "default_entry": "hosted",
            **identity,
            "entries": [{"id": "hosted"}],
            "artifacts": [
                {
                    "iso_path": path,
                    "role": role,
                    **identity,
                }
                for path, role in (
                    ("/boot/hosted/initramfs.cpio.gz", "linux-initrd"),
                    ("/boot/hosted/vmlinuz-lts", "linux-kernel"),
                )
            ],
            "capacity_lock_bytes": 7,
            "capacity_lock_sha256": "4" * 64,
            "efi_boot_image_bytes": 7,
            "efi_boot_image_sha256": "4" * 64,
            "efi_bootloader_bytes": 7,
            "efi_bootloader_sha256": "4" * 64,
            "grub_config_bytes": 7,
            "grub_config_sha256": "4" * 64,
        }
        native_path = {
            "filesystem": "ext4",
            "requested": "/home/ubuntu/release",
            "resolved": "/home/ubuntu/release",
            "mount_point": "/",
            "ownership_anchor": "/home/ubuntu",
            "owner_uid": 1000,
            "owner_gid": 1000,
            "guest_uid": 1000,
            "mode": 0o755,
        }
        payload = {
            "schema": "ostadix.hosted-live-release/v2",
            "source": {
                "staged_tree": snapshot.tree,
                "base_commit": snapshot.head,
                "archive_sha256": snapshot.archive_sha256,
            },
            "build": {
                "host_architecture": "aarch64",
                "target": "x86_64-unknown-linux-musl",
                "rust_toolchain": "rustc 1.97.1 (fixture)",
                "cargo_build_jobs": 1,
                "cargo_codegen_units": 16,
                "cargo_lto": False,
                "source_date_epoch": 315532800,
                "musl_dev_version": "1.2.6-r2",
                "sysroot_package_lock": list(RELEASE.EXPECTED_SYSROOT_PACKAGE_LOCK),
                "sysroot_manifest": dict(identity),
                "hosted_live_package_lock": dict(identity),
                "apk_repository_boundary": {
                    "exact_versions": True,
                    "signed_index_and_packages": True,
                    "independent_apk_blob_hash_lock": False,
                    "repository_availability_required": True,
                },
                "cache_inputs": {
                    "alpine_minirootfs": {
                        "bytes": RELEASE.PINNED_MINIROOTFS_BYTES,
                        "sha256": RELEASE.PINNED_MINIROOTFS_SHA256,
                    },
                    "alpine_lts_kernel": {
                        "bytes": RELEASE.PINNED_LTS_KERNEL_BYTES,
                        "sha256": RELEASE.PINNED_LTS_KERNEL_SHA256,
                    },
                    "alpine_lts_initramfs": {
                        "bytes": RELEASE.PINNED_LTS_INITRAMFS_BYTES,
                        "sha256": RELEASE.PINNED_LTS_INITRAMFS_SHA256,
                    },
                },
            },
            "binaries": {
                name: dict(identity) for name in ("O", "o-cli", "olangc", "o-link")
            },
            "initramfs": dict(identity),
            "iso": inspection,
            "smoke": {
                "schema": "ostadix.hosted-live-boot-gates/v2",
                "serial": {
                    "schema": "ostadix.hosted-live-qemu-smoke/v1",
                    "markers": list(RELEASE.REQUIRED_SMOKE_MARKERS),
                    "transcript_bytes": 7,
                    "transcript_sha256": "4" * 64,
                    "exit_code": 0,
                    "acceleration": "tcg",
                    "firmware_path": "ovmf-through-capacity-runner",
                    "physical_hardware_proof": False,
                },
                "graphical": {
                    "schema": "ostadix.hosted-live-qemu-visual-smoke/v1",
                    "markers": list(RELEASE.REQUIRED_SMOKE_MARKERS),
                    "input_marker": "vga-input-pass",
                    "serial": dict(identity),
                    "frame_before": {
                        **identity,
                        "width": 640,
                        "height": 480,
                        "nonblack_pixels": 3000,
                        "unique_colors": 2,
                    },
                    "frame_after": {
                        **identity,
                        "width": 640,
                        "height": 480,
                        "nonblack_pixels": 3200,
                        "unique_colors": 2,
                    },
                    "changed_pixels": 300,
                    "acceleration": "tcg",
                    "firmware": dict(identity),
                    "display_device": "VGA",
                    "input_device": "usb-kbd",
                    "network": "none",
                    "physical_hardware_proof": False,
                },
            },
            "boot_profile": {
                "kind": "physical-hosted-live",
                "kernel_flavor": "alpine-lts",
                "preferred_console": "tty0",
                "panic_timeout_seconds": 0,
                "ventoy_mode": "grub2-filename-suffix",
            },
            "claim_boundary": {
                "substrate": "fixture",
                "physical_hardware_proof": False,
                "secure_boot_proof": False,
                "hermetic": False,
                "host_mounts_may_be_visible": True,
            },
            "guest_path_boundary": {
                "native_paths": [native_path],
                "hermetic": False,
                "host_mounts_may_be_visible": True,
            },
            "host_publication": {
                "output": str(output),
                "receipt": str(receipt),
                **identity,
                "branch": snapshot.branch,
                "origin": snapshot.origin,
                "published_utc": "2026-08-28T00:00:00+00:00",
            },
        }
        return inspection, payload

    def test_snapshot_binds_staged_tree_and_excludes_untracked_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "repo"
            root.mkdir()
            self.repository(root)
            (root / "tracked.txt").write_text("staged\n", encoding="utf-8")
            self.git(root, "add", "tracked.txt")
            (root / "INVENTORY.md").write_text("untracked\n", encoding="utf-8")
            archive = Path(directory) / "source.tar"

            snapshot = RELEASE.create_source_snapshot(root, archive)
            second = RELEASE.create_source_snapshot(
                root, Path(directory) / "source-second.tar"
            )

            self.assertEqual(snapshot.tree, self.git(root, "write-tree"))
            self.assertEqual(snapshot.head, self.git(root, "rev-parse", "HEAD"))
            self.assertNotEqual(snapshot.tree, self.git(root, "rev-parse", "HEAD^{tree}"))
            with tarfile.open(archive, "r:") as source:
                names = set(source.getnames())
                tracked = source.extractfile("tracked.txt")
                assert tracked is not None
                self.assertEqual(tracked.read(), b"staged\n")
            self.assertNotIn("INVENTORY.md", names)
            self.assertEqual(snapshot.archive_sha256, second.archive_sha256)
            with tarfile.open(archive, "r:") as source:
                self.assertTrue(all(member.mtime == 315532800 for member in source))

    def test_snapshot_rejects_unstaged_tracked_changes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "repo"
            root.mkdir()
            self.repository(root)
            (root / "tracked.txt").write_text("unstaged\n", encoding="utf-8")
            with self.assertRaisesRegex(RELEASE.ReleaseError, "unstaged changes"):
                RELEASE.create_source_snapshot(root, Path(directory) / "source.tar")

    def test_snapshot_rejects_external_source_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "repo"
            root.mkdir()
            self.repository(root)
            os.symlink("/Users/ustad/Ostadix-lang", root / "external-source")
            self.git(root, "add", "external-source")
            with self.assertRaisesRegex(RELEASE.ReleaseError, "symlinks"):
                RELEASE.create_source_snapshot(root, Path(directory) / "source.tar")

    def test_no_clobber_rejects_existing_iso_or_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "hosted.iso"
            output.write_bytes(b"existing")
            with self.assertRaisesRegex(RELEASE.ReleaseError, "refusing to clobber"):
                RELEASE.validate_no_clobber(output)
            output.unlink()
            RELEASE.receipt_path_for(output).write_text("{}\n", encoding="utf-8")
            with self.assertRaisesRegex(RELEASE.ReleaseError, "refusing to clobber"):
                RELEASE.validate_no_clobber(output)

    def test_default_output_is_bound_to_the_staged_tree(self) -> None:
        output = RELEASE.default_output_for("a" * 40)
        self.assertEqual(
            output.name,
            "ostadix-hosted-live-x86_64-uefi-aaaaaaaaaaaa_VTGRUB2.iso",
        )
        self.assertEqual(output.parent, RELEASE.DEFAULT_OUTPUT_DIRECTORY)

    def test_guest_archive_hash_is_verified_before_extraction(self) -> None:
        class Client:
            def execute(self, _arguments, **_kwargs):
                return subprocess.CompletedProcess(
                    [], 0, stdout=f"{'0' * 64}  staged-source.tar\n", stderr=""
                )

        with self.assertRaisesRegex(RELEASE.ReleaseError, "guest SHA-256"):
            RELEASE.verify_guest_archive(
                Client(), Path("/guest/staged-source.tar"), "1" * 64
            )

    def test_guest_release_paths_reject_host_mounted_filesystems(self) -> None:
        class Client:
            def execute(self, _arguments, **_kwargs):
                return subprocess.CompletedProcess(
                    [],
                    0,
                    stdout=json.dumps(
                        [
                            {
                                "requested": "/guest/run",
                                "resolved": "/guest/run",
                                "mount_point": "/guest",
                                "filesystem": "fuse.sshfs",
                                "ownership_anchor": "/guest",
                                "owner_uid": 1000,
                                "owner_gid": 1000,
                                "guest_uid": 1000,
                                "mode": 0o700,
                            }
                        ]
                    ),
                    stderr="",
                )

        with self.assertRaisesRegex(RELEASE.ReleaseError, "native filesystem"):
            RELEASE.verify_guest_native_paths(Client(), [Path("/guest/run")])

    def test_guest_release_paths_reject_non_native_shared_filesystems(self) -> None:
        for filesystem in ("9p", "virtiofs", "nfs4", "cifs"):
            class Client:
                def execute(self, _arguments, **_kwargs):
                    return subprocess.CompletedProcess(
                        [],
                        0,
                        stdout=json.dumps(
                            [
                                {
                                    "requested": "/guest/run",
                                    "resolved": "/guest/run",
                                    "mount_point": "/guest",
                                    "filesystem": filesystem,
                                    "ownership_anchor": "/guest",
                                    "owner_uid": 1000,
                                    "owner_gid": 1000,
                                    "guest_uid": 1000,
                                    "mode": 0o700,
                                }
                            ]
                        ),
                        stderr="",
                    )

            with self.subTest(filesystem=filesystem), self.assertRaisesRegex(
                RELEASE.ReleaseError, "native filesystem"
            ):
                RELEASE.verify_guest_native_paths(Client(), [Path("/guest/run")])

    def test_guest_cleanup_failure_does_not_invalidate_publication(self) -> None:
        class Client:
            def execute(self, _arguments, **_kwargs):
                raise RELEASE.ReleaseError("fixture cleanup failure")

        errors = io.StringIO()
        with contextlib.redirect_stderr(errors):
            RELEASE._best_effort_guest_cleanup(
                Client(),
                Path("/guest/private-run"),
                publication_succeeded=True,
            )
        self.assertIn("published release remains valid", errors.getvalue())

    def test_release_failure_cleans_the_exact_random_guest_run(self) -> None:
        snapshot = RELEASE.SourceSnapshot(
            tree="1" * 40,
            head="2" * 40,
            branch="main",
            origin=RELEASE.CANONICAL_REMOTE,
            archive=Path("/host/staged-source.tar"),
            archive_sha256="3" * 64,
        )

        class Client:
            def __init__(self):
                self.calls = []

            def ensure_running(self):
                return {"state": "Running"}

            def execute(self, arguments, **kwargs):
                self.calls.append((arguments, kwargs))
                if arguments[:2] == ["python3", "-c"]:
                    return subprocess.CompletedProcess(
                        arguments,
                        0,
                        stdout=str(20 * 1024 * 1024 * 1024),
                        stderr="",
                    )
                return subprocess.CompletedProcess(arguments, 0, stdout="", stderr="")

        client = Client()
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            RELEASE, "create_source_snapshot", return_value=snapshot
        ), mock.patch.object(
            RELEASE, "MultipassClient", return_value=client
        ), mock.patch.object(
            RELEASE, "verify_guest_native_paths", return_value=[]
        ), mock.patch.object(
            RELEASE, "_complete_guest_release", side_effect=RELEASE.ReleaseError("fixture")
        ):
            with self.assertRaisesRegex(RELEASE.ReleaseError, "fixture"):
                RELEASE.release(
                    Path(directory) / "output.iso",
                    multipass_executable="multipass",
                )
        cleanup_calls = [
            call
            for call in client.calls
            if call[0][:5] == ["sudo", "-n", "rm", "-rf", "--"]
        ]
        self.assertEqual(len(cleanup_calls), 1)
        self.assertEqual(cleanup_calls[0][1], {"capture_output": False})
        self.assertRegex(
            cleanup_calls[0][0][5],
            r"^/home/ubuntu/\.cache/ostadix/hosted-live-release/runs/111111111111-[0-9a-f]{16}$",
        )

    def test_release_rejects_a_missing_canonical_origin(self) -> None:
        snapshot = RELEASE.SourceSnapshot(
            tree="1" * 40,
            head="2" * 40,
            branch="main",
            origin="",
            archive=Path("/host/staged-source.tar"),
            archive_sha256="3" * 64,
        )
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            RELEASE, "create_source_snapshot", return_value=snapshot
        ):
            with self.assertRaisesRegex(RELEASE.ReleaseError, "canonical release"):
                RELEASE.release(Path(directory) / "output.iso", multipass_executable="x")

    def test_publication_interrupt_rolls_back_new_orphan_receipt(self) -> None:
        snapshot = RELEASE.SourceSnapshot(
            tree="1" * 40,
            head="2" * 40,
            branch="main",
            origin=RELEASE.CANONICAL_REMOTE,
            archive=Path("/host/staged-source.tar"),
            archive_sha256="3" * 64,
        )
        inspection = {"sha256": "4" * 64, "bytes": 7}
        payload = {
            "schema": "ostadix.hosted-live-release/v2",
            "source": {
                "staged_tree": snapshot.tree,
                "base_commit": snapshot.head,
                "archive_sha256": snapshot.archive_sha256,
            },
            "iso": dict(inspection),
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "output.iso"
            receipt = RELEASE.receipt_path_for(output)
            with mock.patch.object(
                RELEASE, "_invoke", side_effect=KeyboardInterrupt
            ), mock.patch.object(
                RELEASE,
                "_strict_inspect",
                side_effect=RELEASE.ReleaseError("output absent"),
            ):
                with self.assertRaises(KeyboardInterrupt):
                    RELEASE._publish_verified_release(
                        candidate=root / "candidate.iso",
                        output=output,
                        receipt=receipt,
                        inspection=inspection,
                        payload=payload,
                        snapshot=snapshot,
                    )
            self.assertFalse(output.exists())
            self.assertFalse(receipt.exists())

    def test_receipt_only_crash_state_is_resumed_without_clobber(self) -> None:
        snapshot = RELEASE.SourceSnapshot(
            tree="1" * 40,
            head="2" * 40,
            branch="main",
            origin=RELEASE.CANONICAL_REMOTE,
            archive=Path("/host/staged-source.tar"),
            archive_sha256="3" * 64,
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "output.iso"
            receipt = RELEASE.receipt_path_for(output)
            inspection, existing = self.complete_receipt(
                snapshot, output, receipt
            )
            RELEASE._exclusive_json(receipt, existing)

            def publish(_arguments):
                receipt.unlink()
                output.write_bytes(b"fixture")
                return subprocess.CompletedProcess([], 0, stdout="", stderr="")

            with mock.patch.object(RELEASE, "_invoke", side_effect=publish):
                result = RELEASE._publish_verified_release(
                    candidate=root / "candidate.iso",
                    output=output,
                    receipt=receipt,
                    inspection=inspection,
                    payload={"different": "new guest receipt"},
                    snapshot=snapshot,
                )
            self.assertEqual(result, existing)
            self.assertTrue(RELEASE.publication_lock_path_for(output).is_file())
            self.assertEqual(
                json.loads(receipt.read_text(encoding="utf-8")), existing
            )

    def test_truncated_receipt_cannot_bypass_build_and_smoke_admission(self) -> None:
        snapshot = RELEASE.SourceSnapshot(
            tree="1" * 40,
            head="2" * 40,
            branch="main",
            origin=RELEASE.CANONICAL_REMOTE,
            archive=Path("/host/staged-source.tar"),
            archive_sha256="3" * 64,
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "output.iso"
            receipt = RELEASE.receipt_path_for(output)
            inspection, complete = self.complete_receipt(snapshot, output, receipt)
            output.write_bytes(b"fixture")
            truncated = {
                key: complete[key]
                for key in ("schema", "source", "iso", "host_publication")
            }
            RELEASE._exclusive_json(receipt, truncated)
            with mock.patch.object(RELEASE, "_strict_inspect", return_value=inspection):
                with self.assertRaisesRegex(RELEASE.ReleaseError, "hosted build"):
                    RELEASE._adopt_existing_pair(output, receipt, snapshot)

    def test_linux_worker_binds_reproducibility_and_guest_safety_controls(self) -> None:
        worker = (
            ROOT / "scripts/build-x86_64-hosted-live-linux.sh"
        ).read_text(encoding="utf-8")
        self.assertIn('SYSROOT="$RUN_ROOT/sysroot-', worker)
        self.assertNotIn('SYSROOT="$SHARED_CACHE/sysroots/', worker)
        self.assertIn("CARGO_BUILD_JOBS=1", worker)
        self.assertIn("CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16", worker)
        self.assertIn("CARGO_PROFILE_RELEASE_LTO=false", worker)
        self.assertIn('SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \\', worker)
        self.assertIn('"sysroot_manifest": identity(Path(sysroot_manifest_text))', worker)
        self.assertIn('exec 9<"$SHARED_CACHE"', worker)
        self.assertIn("grep -qx enabled /proc/sys/fs/binfmt_misc/qemu-x86_64", worker)
        self.assertIn("trap 'exit 130' INT", worker)
        self.assertIn("trap 'exit 143' TERM", worker)
        self.assertIn('find "$SOURCE_ROOT" -type l -print -quit', worker)
        self.assertIn("ALPINE_LTS_KERNEL_SHA256=77007123", worker)
        self.assertIn("OSTADIX_CAPACITY_HOST_KERNEL_FLAVOR=lts", worker)
        self.assertIn("build-x86_64-hosted-live-iso.sh", worker)
        self.assertIn("smoke-x86_64-hosted-live-vga-qemu.py", worker)
        self.assertNotIn("foreign_kernel_lab.py", worker)

    def test_vga_gate_rejects_black_and_unchanged_frames(self) -> None:
        self.assertEqual(
            RELEASE.MIN_GRAPHICAL_NONBLACK_PIXELS,
            VGA_SMOKE.MIN_NONBLACK_PIXELS,
        )
        self.assertEqual(
            RELEASE.MIN_GRAPHICAL_UNIQUE_COLORS,
            VGA_SMOKE.MIN_UNIQUE_COLORS,
        )
        self.assertEqual(
            RELEASE.MIN_GRAPHICAL_CHANGED_PIXELS,
            VGA_SMOKE.MIN_CHANGED_PIXELS,
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            width, height = 320, 200
            black_path = root / "black.ppm"
            black_path.write_bytes(b"P6\n320 200\n255\n" + bytes(width * height * 3))
            black = VGA_SMOKE.read_frame(black_path)
            with self.assertRaisesRegex(VGA_SMOKE.VisualSmokeError, "effectively black"):
                VGA_SMOKE.validate_visible_frame(black)

            pixels = bytearray(width * height * 3)
            for pixel in range(3000):
                start = pixel * 3
                pixels[start : start + 3] = b"\xff\xff\xff"
            before_path = root / "before.ppm"
            before_path.write_bytes(b"P6\n320 200\n255\n" + pixels)
            before = VGA_SMOKE.read_frame(before_path)
            self.assertEqual(before.unique_colors, 2)
            VGA_SMOKE.validate_visible_frame(before)
            with self.assertRaisesRegex(VGA_SMOKE.VisualSmokeError, "did not visibly react"):
                VGA_SMOKE.changed_pixel_count(before, before)

            changed = bytearray(pixels)
            for pixel in range(400, 700):
                start = pixel * 3
                changed[start : start + 3] = b"\x7f\x7f\x7f"
            after_path = root / "after.ppm"
            after_path.write_bytes(b"P6\n320 200\n255\n" + changed)
            after = VGA_SMOKE.read_frame(after_path)
            self.assertGreaterEqual(VGA_SMOKE.changed_pixel_count(before, after), 300)

    def test_vga_input_command_is_fully_mappable_to_qemu_keys(self) -> None:
        keys = [VGA_SMOKE._key_name(character) for character in VGA_SMOKE.INPUT_COMMAND]
        self.assertIn("shift-s", keys)
        self.assertIn("shift-dot", keys)
        self.assertEqual(keys[-1], "ret")

    def test_vga_monitor_socket_avoids_deep_evidence_paths(self) -> None:
        directory, path = VGA_SMOKE._allocate_monitor_socket()
        try:
            self.assertLessEqual(
                len(os.fsencode(path)), VGA_SMOKE.MAX_MONITOR_SOCKET_PATH_BYTES
            )
            self.assertEqual(path.name, "qemu.sock")
        finally:
            directory.cleanup()
        self.assertFalse(path.parent.exists())

    def test_vga_hmp_quit_does_not_wait_for_a_prompt(self) -> None:
        monitor = VGA_SMOKE.Hmp.__new__(VGA_SMOKE.Hmp)
        monitor.socket = mock.Mock()
        monitor.quit()
        monitor.socket.sendall.assert_called_once_with(b"quit\n")
        monitor.socket.recv.assert_not_called()

    def test_vga_monitor_wait_reports_an_early_qemu_exit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            qemu_log = root / "qemu.log"
            qemu_log.write_text("qemu-system: bad device\n", encoding="utf-8")
            process = mock.Mock()
            process.poll.return_value = 1
            process.returncode = 1
            with self.assertRaisesRegex(
                VGA_SMOKE.VisualSmokeError, "bad device"
            ):
                VGA_SMOKE._wait_for_monitor(
                    root / "monitor.sock", process, qemu_log, time.monotonic() + 1
                )

    def test_vga_ppm_parser_preserves_whitespace_valued_first_pixel(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "frame.ppm"
            first_pixel = b"\x0a\x20\x09"
            path.write_bytes(
                b"P6\n320 200\n255\n"
                + first_pixel
                + bytes(320 * 200 * 3 - len(first_pixel))
            )
            self.assertEqual(VGA_SMOKE.read_frame(path).pixels[:3], first_pixel)

    def test_vga_fd_reference_uses_a_host_descriptor_filesystem(self) -> None:
        self.assertRegex(VGA_SMOKE._fd_reference(7), r"^/(proc/self|dev)/fd/7$")

    def fake_program(self, root: Path, source: str) -> Path:
        path = root / "fake.py"
        path.write_text(textwrap.dedent(source), encoding="utf-8")
        return path

    def test_marker_gate_requires_all_markers_in_order_and_exits_cleanly(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            script = self.fake_program(
                Path(directory),
                f"""
                import sys
                markers = {list(SMOKE.REQUIRED_MARKERS)!r}
                for marker in markers:
                    sys.stdout.buffer.write(marker + b"\\n")
                    sys.stdout.buffer.flush()
                sys.stdin.buffer.read(2)
                """,
            )
            output = io.BytesIO()
            result = SMOKE.run_marker_gate(
                [sys.executable, str(script)],
                timeout_seconds=2,
                transcript_output=output,
            )
            self.assertEqual(result.exit_code, 0)
            self.assertEqual(result.markers, tuple(item.decode() for item in SMOKE.REQUIRED_MARKERS))
            self.assertIn(SMOKE.REQUIRED_MARKERS[-1], output.getvalue())

    def test_marker_gate_rejects_explicit_failure(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            script = self.fake_program(
                Path(directory),
                """
                import sys
                sys.stdout.write("OSTADIX HOSTED O SMOKE: FAIL: fixture\\n")
                sys.stdout.flush()
                """,
            )
            with self.assertRaisesRegex(SMOKE.SmokeError, "failure marker"):
                SMOKE.run_marker_gate([sys.executable, str(script)], timeout_seconds=2)

    def test_marker_gate_rejects_failure_emitted_after_ready(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            script = self.fake_program(
                Path(directory),
                f"""
                import sys
                markers = {list(SMOKE.REQUIRED_MARKERS)!r}
                for marker in markers:
                    sys.stdout.buffer.write(marker + b"\\n")
                    sys.stdout.buffer.flush()
                sys.stdin.buffer.read(2)
                sys.stdout.write("OSTADIX HOSTED LATE: FAIL: after-ready\\n")
                sys.stdout.flush()
                """,
            )
            with self.assertRaisesRegex(SMOKE.SmokeError, "failure"):
                SMOKE.run_marker_gate([sys.executable, str(script)], timeout_seconds=2)

    def test_marker_gate_times_out_and_reaps_process(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            pid_file = Path(directory) / "pid"
            script = self.fake_program(
                Path(directory),
                """
                import os
                from pathlib import Path
                import sys
                import time
                Path(sys.argv[1]).write_text(str(os.getpid()), encoding="ascii")
                time.sleep(30)
                """,
            )
            started = time.monotonic()
            with self.assertRaisesRegex(SMOKE.SmokeError, "timed out"):
                SMOKE.run_marker_gate(
                    [sys.executable, str(script), str(pid_file)], timeout_seconds=0.1
                )
            self.assertLess(time.monotonic() - started, 3)
            pid = int(pid_file.read_text(encoding="ascii"))
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)

    def test_marker_gate_reaps_descendant_after_runner_exits(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            child_pid_file = Path(directory) / "child-pid"
            script = self.fake_program(
                Path(directory),
                f"""
                from pathlib import Path
                import subprocess
                import sys
                child = subprocess.Popen(["sleep", "30"])
                Path(sys.argv[1]).write_text(str(child.pid), encoding="ascii")
                markers = {list(SMOKE.REQUIRED_MARKERS)!r}
                for marker in markers:
                    sys.stdout.buffer.write(marker + b"\\n")
                    sys.stdout.buffer.flush()
                sys.stdin.buffer.read(2)
                """,
            )
            with self.assertRaisesRegex(SMOKE.SmokeError, "output did not close"):
                SMOKE.run_marker_gate(
                    [sys.executable, str(script), str(child_pid_file)],
                    timeout_seconds=2,
                )
            child_pid = int(child_pid_file.read_text(encoding="ascii"))
            reap_deadline = time.monotonic() + 2
            while True:
                try:
                    os.kill(child_pid, 0)
                except ProcessLookupError:
                    break
                if time.monotonic() >= reap_deadline:
                    self.fail(f"runner descendant remained live after cleanup: {child_pid}")
                time.sleep(0.01)

    def test_multipass_state_commands_are_mockable_and_stopped_vm_is_started(self) -> None:
        calls = []
        states = iter(("Stopped", "Running"))

        def runner(arguments, **_kwargs):
            calls.append(arguments)
            if arguments[1:4] == ["info", "--format", "json"]:
                state = next(states)
                return subprocess.CompletedProcess(
                    arguments,
                    0,
                    stdout=json.dumps({"info": {"fixture": {"state": state}}}),
                    stderr="",
                )
            return subprocess.CompletedProcess(arguments, 0, stdout="", stderr="")

        client = RELEASE.MultipassClient("multipass", "fixture", runner=runner)
        info = client.ensure_running()
        self.assertEqual(info["state"], "Running")
        self.assertIn(["multipass", "start", "fixture"], calls)


if __name__ == "__main__":
    unittest.main()
