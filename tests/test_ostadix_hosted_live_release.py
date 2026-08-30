#!/usr/bin/env python3

import contextlib
import hashlib
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
OCORE_SMOKE = _load(
    "ostadix_hosted_live_ocore_qemu_smoke",
    ROOT / "ocore/kernel/smoke-x86_64-hosted-live-ocore-qemu.py",
)

FIXTURE_ROOTFS_IDENTITY = {"bytes": 7, "sha256": "4" * 64}
FIXTURE_ENTROPY_EVIDENCE = {
    "device": "virtio-rng-pci",
    "crng_bytes": 32,
    "available": 256,
}
FIXTURE_WASM_TREE = "1" * 40
FIXTURE_WASM_PROJECT_SHA256 = "8" * 64
FIXTURE_WASM_EVIDENCE = {
    "staged_tree": FIXTURE_WASM_TREE,
    "bytes": 7,
    "sha256": "4" * 64,
    "materialized_project_sha256": FIXTURE_WASM_PROJECT_SHA256,
}


def _complete_hosted_markers(markers: tuple[bytes, ...]) -> list[bytes]:
    complete = list(markers)
    complete[0] = (
        b"OSTADIX HOSTED ROOTFS: PASS bytes=7 sha256=" + b"4" * 64
    )
    entropy = complete.index(SMOKE.ENTROPY_ORDERED_MARKER)
    complete[entropy] = (
        b"OSTADIX HOSTED ENTROPY: PASS device=virtio-rng-pci "
        b"crng_bytes=32 available=256"
    )
    materialization = complete.index(SMOKE.WASM_MATERIALIZATION_PREFIX.rstrip())
    complete[materialization] = (
        b"OSTADIX HOSTED OLANGC MATERIALIZATION: PASS root_sha256="
        + FIXTURE_WASM_PROJECT_SHA256.encode("ascii")
    )
    artifact = complete.index(SMOKE.WASM_ARTIFACT_PREFIX.rstrip())
    complete[artifact] = (
        b"OSTADIX HOSTED OLANGC WASM ARTIFACT: PASS tree="
        + FIXTURE_WASM_TREE.encode("ascii")
        + b" bytes=7 sha256="
        + b"4" * 64
    )
    return complete


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

    def test_smoke_timeout_policy_is_bounded_and_explicitly_forwarded(self) -> None:
        defaults = RELEASE.resolve_smoke_timeout_policy({})
        self.assertEqual(defaults.hosted_seconds, "1800")
        self.assertEqual(defaults.ocore_seconds, "900")

        lowered = RELEASE.resolve_smoke_timeout_policy(
            {
                "OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT": "1200.5",
                "OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT": "90",
            }
        )
        self.assertEqual(lowered.hosted_seconds, "1200.5")
        self.assertEqual(lowered.ocore_seconds, "90")

        snapshot = RELEASE.SourceSnapshot(
            tree="1" * 40,
            head="2" * 40,
            branch="master",
            origin=RELEASE.CANONICAL_REMOTE,
            archive=Path("/host/source.tar"),
            archive_sha256="3" * 64,
        )
        boot_objects = RELEASE.BootObjectSnapshot(
            archive=Path("/host/objects.tar"),
            archive_sha256="4" * 64,
            summary={},
        )
        forwarded = RELEASE._guest_worker_environment(
            snapshot=snapshot,
            boot_objects=boot_objects,
            guest_boot_objects_archive=Path("/guest/objects.tar"),
            smoke_timeouts=lowered,
        )
        self.assertIn("OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT=1200.5", forwarded)
        self.assertIn("OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT=90", forwarded)

        invalid = (
            ("OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT", "nan"),
            ("OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT", "0"),
            ("OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT", "1800.1"),
            ("OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT", "inf"),
            ("OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT", "901"),
        )
        for name, value in invalid:
            with self.subTest(name=name, value=value), self.assertRaisesRegex(
                RELEASE.ReleaseError, name
            ):
                RELEASE.resolve_smoke_timeout_policy({name: value})

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
            "entries": [
                {
                    "id": "hosted",
                    "title": "OSTADIX Hosted Workstation [physical x86_64]",
                    "hotkey": "h",
                    "adapter": "linux-live-rootfs",
                    "arguments": [
                        "console=ttyS0,115200n8",
                        "console=tty0",
                        "rdinit=/init",
                        "panic=0",
                        "loglevel=7",
                        "ignore_loglevel",
                    ],
                    "kernel_path": "/boot/hosted/vmlinuz-lts",
                    "initrd_paths": ["/boot/hosted/initramfs.cpio.gz"],
                    "selection_id": "hosted",
                    "rootfs_path": "/boot/hosted/rootfs.squashfs",
                    "modloop_path": "/boot/modloop-lts",
                },
                {
                    "id": "ocore",
                    "title": "OSTADIX O-core [direct Multiboot2, serial console]",
                    "hotkey": "o",
                    "adapter": "multiboot2",
                    "arguments": [],
                    "kernel_path": "/boot/ocore/kernel.elf",
                },
                {
                    "id": "alpine",
                    "title": "Alpine Linux 3.24.1 [direct kernel/initramfs]",
                    "hotkey": "a",
                    "adapter": "linux",
                    "arguments": [
                        "console=tty0",
                        "console=ttyS0,115200n8",
                        "rdinit=/bin/sh",
                        "panic=0",
                        "loglevel=4",
                    ],
                    "kernel_path": "/boot/capacity-host/vmlinuz-virt",
                    "initrd_paths": ["/boot/entry/010-alpine/initramfs-virt"],
                },
                {
                    "id": "guix",
                    "title": "GNU Guix System 1.5.0 [virtualized/TCG]",
                    "hotkey": "g",
                    "adapter": "qemu-tcg-linux-direct",
                    "arguments": [
                        "console=tty0",
                        "console=ttyS0,115200n8",
                        "rdinit=/init",
                        "panic=0",
                        "loglevel=4",
                    ],
                    "host_kernel_path": "/boot/capacity-host/vmlinuz-virt",
                    "host_initrd_path": "/boot/capacity-host/initramfs.cpio.gz",
                    "selection_id": "guix-system-1.5.0-x86_64",
                    "guest_artifact_paths": [
                        "/ostadix/guix/linux-libre-6.17.12-bzimage",
                        "/ostadix/guix/guix-1.5.0-initrd.cpio.gz",
                        "/ostadix/guix/guix-system-install-1.5.0.x86_64-linux.iso",
                    ],
                },
                {
                    "id": "openbsd",
                    "title": "OpenBSD 7.9 offline installer [virtualized/TCG]",
                    "hotkey": "b",
                    "adapter": "qemu-tcg-raw-cd-curses",
                    "arguments": [
                        "console=tty0",
                        "console=ttyS0,115200n8",
                        "rdinit=/init",
                        "panic=0",
                        "loglevel=4",
                    ],
                    "host_kernel_path": "/boot/capacity-host/vmlinuz-virt",
                    "host_initrd_path": "/boot/capacity-host/initramfs.cpio.gz",
                    "selection_id": "openbsd-7.9-amd64",
                    "guest_artifact_paths": ["/ostadix/openbsd/install79.iso"],
                },
                {
                    "id": "plan9",
                    "title": "9front Plan 9 build 11983 [virtualized/TCG]",
                    "hotkey": "p",
                    "adapter": "qemu-tcg-qcow2",
                    "arguments": [
                        "console=tty0",
                        "console=ttyS0,115200n8",
                        "rdinit=/init",
                        "panic=0",
                        "loglevel=4",
                    ],
                    "host_kernel_path": "/boot/capacity-host/vmlinuz-virt",
                    "host_initrd_path": "/boot/capacity-host/initramfs.cpio.gz",
                    "selection_id": "plan9-9front-11983-amd64",
                    "guest_artifact_paths": [
                        "/ostadix/9front/9front-11983.amd64.qcow2"
                    ],
                },
                {
                    "id": "redox",
                    "title": "Redox OS 0.9.0 [virtualized/TCG]",
                    "hotkey": "r",
                    "adapter": "qemu-tcg-raw-cd",
                    "arguments": [
                        "console=tty0",
                        "console=ttyS0,115200n8",
                        "rdinit=/init",
                        "panic=0",
                        "loglevel=4",
                    ],
                    "host_kernel_path": "/boot/capacity-host/vmlinuz-virt",
                    "host_initrd_path": "/boot/capacity-host/initramfs.cpio.gz",
                    "selection_id": "redox-0.9.0-server-x86_64",
                    "guest_artifact_paths": [
                        "/ostadix/redox/redox-server-0.9.0-livedisk.iso"
                    ],
                },
            ],
            "artifacts": [
                {
                    "iso_path": path,
                    "role": role,
                    **(
                        {
                            "bytes": RELEASE.PINNED_LTS_KERNEL_BYTES,
                            "sha256": RELEASE.PINNED_LTS_KERNEL_SHA256,
                        }
                        if path == "/boot/hosted/vmlinuz-lts"
                        else identity
                    ),
                }
                for path, role in (
                    ("/boot/hosted/initramfs.cpio.gz", "linux-initrd"),
                    ("/boot/hosted/vmlinuz-lts", "linux-kernel"),
                    ("/boot/hosted/rootfs.squashfs", "linux-rootfs"),
                    ("/boot/modloop-lts", "linux-modloop"),
                    ("/boot/ocore/kernel.elf", "ocore-kernel"),
                    ("/boot/capacity-host/vmlinuz-virt", "linux-kernel"),
                    ("/boot/capacity-host/initramfs.cpio.gz", "linux-initrd"),
                    ("/boot/entry/010-alpine/initramfs-virt", "linux-initrd"),
                    ("/ostadix/guix/linux-libre-6.17.12-bzimage", "linux-kernel"),
                    ("/ostadix/guix/guix-1.5.0-initrd.cpio.gz", "linux-initrd"),
                    (
                        "/ostadix/guix/guix-system-install-1.5.0.x86_64-linux.iso",
                        "guest-rootfs",
                    ),
                    ("/ostadix/openbsd/install79.iso", "guest-raw-cd"),
                    ("/ostadix/9front/9front-11983.amd64.qcow2", "guest-qcow2"),
                    (
                        "/ostadix/redox/redox-server-0.9.0-livedisk.iso",
                        "guest-raw-cd",
                    ),
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
        wasm_descriptor = {
            "schema": "ostadix.olangc-wasm-release/v1",
            "source": {
                "staged_tree": snapshot.tree,
                "base_commit": snapshot.head,
                "archive_sha256": snapshot.archive_sha256,
            },
            "input": {
                "path": "examples/wasm_hello.O",
                **identity,
            },
            "generator": {
                "path": "/usr/local/bin/olangc",
                **identity,
            },
            "project": {
                "file_count": 2,
                "logical_bytes": 10,
                "root_sha256": FIXTURE_WASM_PROJECT_SHA256,
            },
            "artifact": {
                "path": "/usr/share/ostadix/wasm/hello.wasm",
                **identity,
            },
            "build": {
                "target": "wasm32-wasip1",
                "profile": "release",
                "opt_level": 1,
                "lto": False,
                "codegen_units": 16,
                "cargo_offline": True,
                "rust_toolchain": "rustc 1.97.1 (fixture)",
            },
        }
        wasm_evidence = {
            "staged_tree": snapshot.tree,
            "bytes": identity["bytes"],
            "sha256": identity["sha256"],
            "materialized_project_sha256": FIXTURE_WASM_PROJECT_SHA256,
        }
        canonical_wasm_descriptor = (
            json.dumps(wasm_descriptor, indent=2, sort_keys=True) + "\n"
        ).encode("utf-8")
        wasm_manifest_identity = {
            "bytes": len(canonical_wasm_descriptor),
            "sha256": hashlib.sha256(canonical_wasm_descriptor).hexdigest(),
        }
        guest_record_keys = (
            ("linux-alpine-3.24.1-x86_64", "kernel"),
            ("linux-alpine-3.24.1-x86_64", "initramfs"),
            ("guix-system-1.5.0-x86_64", "media"),
            ("guix-system-1.5.0-x86_64", "media_signature"),
            ("guix-system-1.5.0-x86_64", "kernel"),
            ("guix-system-1.5.0-x86_64", "initrd"),
            ("plan9-9front-11983-amd64", "disk_gz"),
            ("plan9-9front-11983-amd64", "disk"),
            ("redox-0.9.0-server-x86_64", "media_zst"),
            ("redox-0.9.0-server-x86_64", "media"),
            ("openbsd-7.9-amd64", "media"),
        )
        guest_records = [
            f"verified guest={guest} artifact={artifact} size=7 sha256={'4' * 64}"
            for guest, artifact in guest_record_keys
        ]
        encoded_guest_records = ("\n".join(guest_records) + "\n").encode("utf-8")
        guest_verification_identity = {
            "bytes": len(encoded_guest_records),
            "sha256": hashlib.sha256(encoded_guest_records).hexdigest(),
        }
        payload = {
            "schema": "ostadix.hosted-live-release/v6",
            "source": {
                "staged_tree": snapshot.tree,
                "base_commit": snapshot.head,
                "archive_sha256": snapshot.archive_sha256,
                "boot_objects_archive_sha256": "5" * 64,
                "boot_objects": {
                    "schema": "ostadix.boot-object-store-result/v1",
                    "ok": True,
                    "operation": "verify",
                    "commit": snapshot.head,
                    "tree": snapshot.tree,
                    "root_sha256": "6" * 64,
                    "object_count": 8,
                    "binding_count": 9,
                    "logical_bytes": 11,
                    "stored_bytes": 10,
                },
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
                "workstation_package_roots": list(
                    RELEASE.EXPECTED_WORKSTATION_PACKAGE_ROOTS
                ),
                "workstation_source_path": "/usr/src/ostadix",
                "sysroot_package_lock": list(RELEASE.EXPECTED_SYSROOT_PACKAGE_LOCK),
                "sysroot_manifest": dict(identity),
                "hosted_live_package_lock": dict(identity),
                "cargo_vendor_manifest": dict(identity),
                "ocore": {
                    "compiler_target": "x86_64-unknown-none",
                    "assembler_target": "x86_64-unknown-none-elf",
                    "probe_mode": 0,
                    "boot_info_enabled": True,
                    "linker": "ld.lld",
                    "cargo_build_jobs": 1,
                    "cargo_offline": True,
                },
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
                    "alpine_lts_modloop": {
                        "bytes": RELEASE.PINNED_LTS_MODLOOP_BYTES,
                        "sha256": RELEASE.PINNED_LTS_MODLOOP_SHA256,
                    },
                },
            },
            "binaries": {
                name: dict(identity) for name in RELEASE.REQUIRED_HOSTED_BINARIES
            },
            "rootfs_objects": {
                "olangc_wasm_hello": {
                    "manifest_path": "/usr/share/ostadix/wasm/hello.release.json",
                    "artifact_path": "/usr/share/ostadix/wasm/hello.wasm",
                    "manifest": wasm_manifest_identity,
                    "descriptor": wasm_descriptor,
                }
            },
            "capacity": {
                "host_initramfs": dict(identity),
                "foreign_manifest": dict(identity),
                "package_lock": dict(identity),
                "guest_verification": {
                    "identity": guest_verification_identity,
                    "records": guest_records,
                },
                "virt_modloop": {
                    "bytes": RELEASE.PINNED_VIRT_MODLOOP_BYTES,
                    "sha256": RELEASE.PINNED_VIRT_MODLOOP_SHA256,
                },
                "boot_routes": {
                    "direct": ["hosted", "ocore", "alpine"],
                    "nested_qemu_tcg": ["guix", "openbsd", "plan9", "redox"],
                },
            },
            "initramfs": dict(identity),
            "rootfs": dict(identity),
            "ventoy_modloop": dict(identity),
            "ocore_kernel": dict(identity),
            "iso": inspection,
            "smoke": {
                "schema": "ostadix.hosted-live-boot-gates/v6",
                "serial": {
                    "schema": "ostadix.hosted-live-qemu-smoke/v4",
                    "markers": list(RELEASE.REQUIRED_SMOKE_MARKERS),
                    "transcript_bytes": 7,
                    "transcript_sha256": "4" * 64,
                    "exit_code": 0,
                    "iso": dict(identity),
                    "rootfs": dict(identity),
                    "acceleration": "tcg",
                    "entropy": dict(FIXTURE_ENTROPY_EVIDENCE),
                    "olangc_wasm": dict(wasm_evidence),
                    "firmware_path": "ovmf-through-capacity-runner",
                    "physical_hardware_proof": False,
                },
                "graphical": {
                    "schema": "ostadix.hosted-live-qemu-visual-smoke/v7",
                    "markers": list(RELEASE.REQUIRED_VISUAL_SMOKE_MARKERS),
                    "font_marker": "OSTADIX HOSTED X11 FONT: PASS",
                    "pty_marker": "OSTADIX HOSTED PTY: PASS",
                    "evdev_marker": "OSTADIX HOSTED EVDEV: PASS",
                    "notebook_gui_marker": "OSTADIX HOSTED NOTEBOOK GUI READY: PASS",
                    "desktop_marker": "OSTADIX HOSTED DESKTOP READY: PASS",
                    "input_marker": "vga-input-pass",
                    "session": "openbox-xterm",
                    "iso": dict(identity),
                    "rootfs": dict(identity),
                    "serial": dict(identity),
                    "frame_before": {
                        **identity,
                        "width": 640,
                        "height": 480,
                        "nonblack_pixels": 30000,
                        "unique_colors": 16,
                        "chromatic_pixels": 1000,
                        "chromatic_hue_buckets": 4,
                    },
                    "frame_after": {
                        **identity,
                        "width": 640,
                        "height": 480,
                        "nonblack_pixels": 32000,
                        "unique_colors": 16,
                        "chromatic_pixels": 1200,
                        "chromatic_hue_buckets": 4,
                    },
                    "changed_pixels": 300,
                    "acceleration": "tcg",
                    "firmware": dict(identity),
                    "display_device": "VGA",
                    "input_device": "usb-kbd",
                    "entropy": dict(FIXTURE_ENTROPY_EVIDENCE),
                    "olangc_wasm": dict(wasm_evidence),
                    "network": "none",
                    "visual_thresholds": {
                        "minimum_nonblack_pixels": RELEASE.MIN_GRAPHICAL_NONBLACK_PIXELS,
                        "minimum_unique_colors": RELEASE.MIN_GRAPHICAL_UNIQUE_COLORS,
                        "minimum_chromatic_pixels": RELEASE.MIN_GRAPHICAL_CHROMATIC_PIXELS,
                        "minimum_chromatic_hue_buckets": (
                            RELEASE.MIN_GRAPHICAL_CHROMATIC_HUE_BUCKETS
                        ),
                        "minimum_pixels_per_hue_bucket": (
                            RELEASE.MIN_GRAPHICAL_PIXELS_PER_HUE_BUCKET
                        ),
                        "minimum_chromatic_max_channel": (
                            RELEASE.MIN_GRAPHICAL_CHROMATIC_MAX_CHANNEL
                        ),
                        "minimum_chromatic_channel_spread": (
                            RELEASE.MIN_GRAPHICAL_CHROMATIC_CHANNEL_SPREAD
                        ),
                        "minimum_changed_pixels": RELEASE.MIN_GRAPHICAL_CHANGED_PIXELS,
                    },
                    "physical_hardware_proof": False,
                },
                "ocore": {
                    "schema": "ostadix.hosted-live-ocore-qemu-smoke/v1",
                    "selected_entry": "ocore",
                    "selection_method": "grub-hotkey-o",
                    "markers": list(RELEASE.REQUIRED_OCORE_SMOKE_MARKERS),
                    "transcript_bytes": 7,
                    "transcript_sha256": "4" * 64,
                    "exit_code": 0,
                    "acceleration": "tcg",
                    "firmware": dict(identity),
                    "iso": dict(identity),
                    "network": "none",
                    "physical_hardware_proof": False,
                },
            },
            "boot_profile": {
                "kind": "physical-hosted-workstation-plus-capacity",
                "default_entry": "hosted",
                "kernel_flavor": "alpine-lts",
                "rootfs_layout": "verified-squashfs-plus-tmpfs-overlay",
                "ventoy_compatibility": "alpine-hook-plus-minimal-modloop",
                "desktop_session": "openbox-xterm",
                "preferred_console": "tty0",
                "panic_timeout_seconds": 0,
                "ocore_entry": "direct-multiboot2-serial",
                "direct_entries": ["hosted", "ocore", "alpine"],
                "nested_qemu_tcg_entries": [
                    "guix",
                    "openbsd",
                    "plan9",
                    "redox",
                ],
                "ventoy_mode": "grub2-filename-suffix",
            },
            "claim_boundary": {
                "substrate": "fixture",
                "physical_hardware_proof": False,
                "secure_boot_proof": False,
                "hermetic": False,
                "host_mounts_may_be_visible": True,
                "foreign_entries_nested_qemu_tcg": True,
                "foreign_entries_direct_grub": False,
                "combined_capacity_menu_execution_proof": False,
                "foreign_guest_gui_proof": False,
                "foreign_guest_package_manager_execution_proof": False,
                "ventoy_foreign_route_proof": False,
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
        boot_objects = RELEASE.BootObjectSnapshot(
            archive=Path("/host/boot-objects.tar"),
            archive_sha256="5" * 64,
            summary={
                "commit": snapshot.head,
                "tree": snapshot.tree,
                "root_sha256": "6" * 64,
                "object_count": 8,
                "binding_count": 9,
                "logical_bytes": 11,
                "stored_bytes": 10,
            },
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
            RELEASE, "create_boot_object_snapshot", return_value=boot_objects
        ), mock.patch.object(
            RELEASE, "assert_snapshot_still_current"
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
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "output.iso"
            receipt = RELEASE.receipt_path_for(output)
            inspection, payload = self.complete_receipt(snapshot, output, receipt)
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

    def test_output_only_recovery_rejects_unvalidated_guest_evidence(self) -> None:
        snapshot = RELEASE.SourceSnapshot(
            tree="1" * 40,
            head="2" * 40,
            branch="main",
            origin=RELEASE.CANONICAL_REMOTE,
            archive=Path("/host/staged-source.tar"),
            archive_sha256="3" * 64,
        )
        for case in ("missing-smoke", "weak-entropy"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                output = root / "output.iso"
                receipt = RELEASE.receipt_path_for(output)
                inspection, payload = self.complete_receipt(snapshot, output, receipt)
                output.write_bytes(b"fixture")
                if case == "missing-smoke":
                    payload.pop("smoke")
                else:
                    payload["smoke"]["serial"]["entropy"]["available"] = 127

                with mock.patch.object(
                    RELEASE, "_strict_inspect", return_value=inspection
                ), mock.patch.object(
                    RELEASE, "_exclusive_json", wraps=RELEASE._exclusive_json
                ) as write_receipt, self.assertRaises(RELEASE.ReleaseError):
                    RELEASE._publish_verified_release(
                        candidate=root / "candidate.iso",
                        output=output,
                        receipt=receipt,
                        inspection=inspection,
                        payload=payload,
                        snapshot=snapshot,
                    )
                write_receipt.assert_not_called()
                self.assertEqual(output.read_bytes(), b"fixture")
                self.assertFalse(receipt.exists())

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

    def test_v5_receipt_is_rejected_instead_of_adopted(self) -> None:
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
            inspection, old = self.complete_receipt(snapshot, output, receipt)
            old["schema"] = "ostadix.hosted-live-release/v5"
            output.write_bytes(b"fixture")
            RELEASE._exclusive_json(receipt, old)
            with mock.patch.object(RELEASE, "_strict_inspect", return_value=inspection):
                with self.assertRaisesRegex(RELEASE.ReleaseError, "unexpected schema"):
                    RELEASE._adopt_existing_pair(output, receipt, snapshot)

    def test_v6_receipt_requires_every_first_class_workstation_layer(self) -> None:
        snapshot = RELEASE.SourceSnapshot(
            tree="1" * 40,
            head="2" * 40,
            branch="main",
            origin=RELEASE.CANONICAL_REMOTE,
            archive=Path("/host/staged-source.tar"),
            archive_sha256="3" * 64,
        )
        output = Path("/host/output.iso")
        receipt = RELEASE.receipt_path_for(output)
        inspection, complete = self.complete_receipt(snapshot, output, receipt)

        self.assertEqual(
            RELEASE.REQUIRED_SMOKE_MARKERS,
            tuple(marker.decode("ascii") for marker in SMOKE.REQUIRED_MARKERS),
        )
        self.assertEqual(
            RELEASE.REQUIRED_VISUAL_SMOKE_MARKERS,
            tuple(marker.decode("ascii") for marker in VGA_SMOKE.REQUIRED_MARKERS),
        )
        self.assertEqual(
            RELEASE.REQUIRED_OCORE_SMOKE_MARKERS,
            tuple(marker.decode("ascii") for marker in OCORE_SMOKE.REQUIRED_MARKERS),
        )
        rootfs_marker = "OSTADIX HOSTED ROOTFS: PASS bytes="
        overlay_marker = "OSTADIX HOSTED ROOTFS OVERLAY: PASS"
        loopback_marker = "OSTADIX HOSTED LOOPBACK: PASS"
        apk_marker = "OSTADIX HOSTED APK: PASS"
        cargo_marker = "OSTADIX HOSTED CARGO HELLO: PASS"
        entropy_marker = "OSTADIX HOSTED ENTROPY: PASS"
        node_marker = "OSTADIX HOSTED O-NODE: PASS"
        source_marker = "OSTADIX HOSTED SOURCE SNAPSHOT: PASS"
        materialization_marker = "OSTADIX HOSTED OLANGC MATERIALIZATION: PASS"
        artifact_marker = "OSTADIX HOSTED OLANGC WASM ARTIFACT: PASS"
        rust_wasm_marker = "OSTADIX HOSTED RUST WASM: PASS"
        wasm_runtime_marker = "OSTADIX HOSTED WASM RUNTIME: PASS"
        wasm_execution_marker = "OSTADIX HOSTED OLANGC WASM EXECUTION: PASS"
        wasm_backend_marker = "OSTADIX HOSTED WEBASSEMBLY BACKEND: PASS"
        self.assertLess(
            RELEASE.REQUIRED_SMOKE_MARKERS.index(rootfs_marker),
            RELEASE.REQUIRED_SMOKE_MARKERS.index(overlay_marker),
        )
        self.assertLess(
            RELEASE.REQUIRED_SMOKE_MARKERS.index(overlay_marker),
            RELEASE.REQUIRED_SMOKE_MARKERS.index(loopback_marker),
        )
        self.assertLess(
            RELEASE.REQUIRED_SMOKE_MARKERS.index(loopback_marker),
            RELEASE.REQUIRED_SMOKE_MARKERS.index(apk_marker),
        )
        self.assertLess(
            RELEASE.REQUIRED_SMOKE_MARKERS.index(cargo_marker),
            RELEASE.REQUIRED_SMOKE_MARKERS.index(entropy_marker),
        )
        self.assertLess(
            RELEASE.REQUIRED_SMOKE_MARKERS.index(entropy_marker),
            RELEASE.REQUIRED_SMOKE_MARKERS.index(node_marker),
        )
        self.assertLess(
            RELEASE.REQUIRED_SMOKE_MARKERS.index(source_marker),
            RELEASE.REQUIRED_SMOKE_MARKERS.index(materialization_marker),
        )
        self.assertLess(
            RELEASE.REQUIRED_SMOKE_MARKERS.index(materialization_marker),
            RELEASE.REQUIRED_SMOKE_MARKERS.index(artifact_marker),
        )
        self.assertLess(
            RELEASE.REQUIRED_SMOKE_MARKERS.index(artifact_marker),
            RELEASE.REQUIRED_SMOKE_MARKERS.index(rust_wasm_marker),
        )
        self.assertLess(
            RELEASE.REQUIRED_SMOKE_MARKERS.index(rust_wasm_marker),
            RELEASE.REQUIRED_SMOKE_MARKERS.index(wasm_runtime_marker),
        )
        self.assertLess(
            RELEASE.REQUIRED_SMOKE_MARKERS.index(wasm_runtime_marker),
            RELEASE.REQUIRED_SMOKE_MARKERS.index(wasm_execution_marker),
        )
        self.assertLess(
            RELEASE.REQUIRED_SMOKE_MARKERS.index(wasm_execution_marker),
            RELEASE.REQUIRED_SMOKE_MARKERS.index(wasm_backend_marker),
        )

        RELEASE._validate_receipt_binding(
            complete,
            output=output,
            receipt=receipt,
            inspection=inspection,
            snapshot=snapshot,
        )

        def clone():
            return json.loads(json.dumps(complete))

        def rebind_wasm_manifest(candidate):
            wasm_object = candidate["rootfs_objects"]["olangc_wasm_hello"]
            encoded = (
                json.dumps(wasm_object["descriptor"], indent=2, sort_keys=True) + "\n"
            ).encode("utf-8")
            wasm_object["manifest"] = {
                "bytes": len(encoded),
                "sha256": hashlib.sha256(encoded).hexdigest(),
            }

        cases = []
        missing_objects = clone()
        del missing_objects["source"]["boot_objects"]
        cases.append((missing_objects, "boot-object"))
        missing_vendor = clone()
        del missing_vendor["build"]["cargo_vendor_manifest"]
        cases.append((missing_vendor, "Cargo vendor"))
        missing_ocore_build = clone()
        del missing_ocore_build["build"]["ocore"]
        cases.append((missing_ocore_build, "O-core build profile"))
        wrong_ocore_kernel = clone()
        wrong_ocore_kernel["ocore_kernel"]["sha256"] = "7" * 64
        cases.append((wrong_ocore_kernel, "built O-core kernel"))
        missing_rootfs = clone()
        del missing_rootfs["rootfs"]
        cases.append((missing_rootfs, "SquashFS root identity"))
        wrong_rootfs = clone()
        wrong_rootfs["rootfs"]["sha256"] = "7" * 64
        cases.append((wrong_rootfs, "different SquashFS root"))
        missing_binary = clone()
        del missing_binary["binaries"]["ostadix-mcp"]
        cases.append((missing_binary, "binary set"))
        missing_rootfs_objects = clone()
        del missing_rootfs_objects["rootfs_objects"]
        cases.append((missing_rootfs_objects, "SquashFS object set"))
        wrong_wasm_source = clone()
        wrong_wasm_source["rootfs_objects"]["olangc_wasm_hello"]["descriptor"][
            "source"
        ]["staged_tree"] = "7" * 40
        rebind_wasm_manifest(wrong_wasm_source)
        cases.append((wrong_wasm_source, "WASM source binding differs"))
        wrong_wasm_generator = clone()
        wrong_wasm_generator["rootfs_objects"]["olangc_wasm_hello"]["descriptor"][
            "generator"
        ]["sha256"] = "7" * 64
        rebind_wasm_manifest(wrong_wasm_generator)
        cases.append((wrong_wasm_generator, "WASM generator differs"))
        wrong_serial_wasm = clone()
        wrong_serial_wasm["smoke"]["serial"]["olangc_wasm"]["sha256"] = "7" * 64
        cases.append((wrong_serial_wasm, "serial Olangc WASM evidence differs"))
        wrong_graphical_materialization = clone()
        wrong_graphical_materialization["smoke"]["graphical"]["olangc_wasm"][
            "materialized_project_sha256"
        ] = "7" * 64
        cases.append(
            (
                wrong_graphical_materialization,
                "graphical Olangc WASM evidence differs",
            )
        )
        missing_capacity = clone()
        del missing_capacity["capacity"]
        cases.append((missing_capacity, "foreign-capacity closure"))
        wrong_capacity_host = clone()
        wrong_capacity_host["capacity"]["host_initramfs"]["sha256"] = "7" * 64
        cases.append((wrong_capacity_host, "capacity-host initramfs differs"))
        wrong_capacity_routes = clone()
        wrong_capacity_routes["capacity"]["boot_routes"]["direct"] = [
            "hosted",
            "ocore",
        ]
        cases.append((wrong_capacity_routes, "direct/QEMU boot-route split"))
        malformed_guest_verification = clone()
        malformed_guest_verification["capacity"]["guest_verification"]["records"] = [
            "not-a-verification-record"
        ]
        cases.append(
            (malformed_guest_verification, "foreign guest verification identity differs")
        )
        overclaimed_ventoy = clone()
        overclaimed_ventoy["claim_boundary"]["ventoy_foreign_route_proof"] = True
        cases.append((overclaimed_ventoy, "hosted-live claim boundary"))
        hosted_only = clone()
        hosted_only["iso"]["entries"] = hosted_only["iso"]["entries"][:1]
        cases.append((hosted_only, "seven-entry boot closure"))
        missing_ocore_gate = clone()
        del missing_ocore_gate["smoke"]["ocore"]
        cases.append((missing_ocore_gate, "firmware boot gates"))
        wrong_ocore_firmware = clone()
        wrong_ocore_firmware["smoke"]["ocore"]["firmware"]["sha256"] = "7" * 64
        cases.append((wrong_ocore_firmware, "different firmware"))
        missing_serial_iso = clone()
        del missing_serial_iso["smoke"]["serial"]["iso"]
        cases.append((missing_serial_iso, "serial smoke ISO"))
        wrong_serial_iso = clone()
        wrong_serial_iso["smoke"]["serial"]["iso"]["sha256"] = "7" * 64
        cases.append((wrong_serial_iso, "serial gate booted a different ISO"))
        missing_graphical_iso = clone()
        del missing_graphical_iso["smoke"]["graphical"]["iso"]
        cases.append((missing_graphical_iso, "graphical ISO"))
        wrong_graphical_iso = clone()
        wrong_graphical_iso["smoke"]["graphical"]["iso"]["sha256"] = "7" * 64
        cases.append((wrong_graphical_iso, "graphical gate booted a different ISO"))
        missing_serial_entropy = clone()
        del missing_serial_entropy["smoke"]["serial"]["entropy"]
        cases.append((missing_serial_entropy, "serial smoke entropy evidence"))
        weak_graphical_entropy = clone()
        weak_graphical_entropy["smoke"]["graphical"]["entropy"]["available"] = 127
        cases.append((weak_graphical_entropy, "graphical smoke entropy evidence"))

        for candidate, message in cases:
            with self.subTest(message=message), self.assertRaisesRegex(
                RELEASE.ReleaseError, message
            ):
                RELEASE._validate_receipt_binding(
                    candidate,
                    output=output,
                    receipt=receipt,
                    inspection=candidate["iso"],
                    snapshot=snapshot,
                )

    def test_adoption_rejects_missing_or_mismatched_rootfs_identity(self) -> None:
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

            cases = []
            missing = json.loads(json.dumps(complete))
            del missing["rootfs"]
            cases.append((missing, "SquashFS root identity"))
            mismatched = json.loads(json.dumps(complete))
            mismatched["rootfs"]["sha256"] = "7" * 64
            cases.append((mismatched, "different SquashFS root"))

            for candidate, message in cases:
                with self.subTest(message=message):
                    receipt.unlink(missing_ok=True)
                    RELEASE._exclusive_json(receipt, candidate)
                    with mock.patch.object(
                        RELEASE, "_strict_inspect", return_value=inspection
                    ), self.assertRaisesRegex(RELEASE.ReleaseError, message):
                        RELEASE._adopt_existing_pair(output, receipt, snapshot)

    def test_adoption_rejects_missing_or_mismatched_smoke_rootfs_identity(
        self,
    ) -> None:
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

            cases = []
            for gate, label in (("serial", "serial"), ("graphical", "graphical")):
                missing = json.loads(json.dumps(complete))
                del missing["smoke"][gate]["rootfs"]
                cases.append((missing, f"QEMU {label}.*rootfs identity"))
                mismatched = json.loads(json.dumps(complete))
                mismatched["smoke"][gate]["rootfs"]["sha256"] = "7" * 64
                cases.append((mismatched, f"{label} gate verified a different SquashFS root"))

            for candidate, message in cases:
                with self.subTest(message=message):
                    receipt.unlink(missing_ok=True)
                    RELEASE._exclusive_json(receipt, candidate)
                    with mock.patch.object(
                        RELEASE, "_strict_inspect", return_value=inspection
                    ), self.assertRaisesRegex(RELEASE.ReleaseError, message):
                        RELEASE._adopt_existing_pair(output, receipt, snapshot)

    def test_adoption_rejects_missing_or_mismatched_ventoy_modloop_identity(
        self,
    ) -> None:
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

            cases = []
            missing = json.loads(json.dumps(complete))
            del missing["ventoy_modloop"]
            cases.append((missing, "Ventoy compatibility modloop identity"))
            mismatched = json.loads(json.dumps(complete))
            mismatched["ventoy_modloop"]["sha256"] = "7" * 64
            cases.append((mismatched, "different Ventoy modloop"))

            for candidate, message in cases:
                with self.subTest(message=message):
                    receipt.unlink(missing_ok=True)
                    RELEASE._exclusive_json(receipt, candidate)
                    with mock.patch.object(
                        RELEASE, "_strict_inspect", return_value=inspection
                    ), self.assertRaisesRegex(RELEASE.ReleaseError, message):
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
        self.assertIn("OCORE_BOOT_INFO_ENABLED=1", worker)
        self.assertIn("OCORE_PROBE_MODE=0", worker)
        self.assertIn('OCORE_LLD="$OCORE_LLD_PATH"', worker)
        self.assertIn('OSTADIX_HOSTED_LIVE_OCORE_KERNEL="$OCORE_KERNEL"', worker)
        self.assertIn("smoke-x86_64-hosted-live-ocore-qemu.py", worker)
        self.assertIn('"schema": "ostadix.hosted-live-boot-gates/v6"', worker)
        self.assertIn('"schema": "ostadix.hosted-live-release/v6"', worker)
        self.assertIn('OSTADIX_CAPACITY_HOST_ROOTFS_OUTPUT="$ROOTFS_IMAGE"', worker)
        self.assertIn('VENTOY_MODLOOP="$RUN_ROOT/output/modloop-lts"', worker)
        self.assertIn(
            'OSTADIX_CAPACITY_HOST_VENTOY_MODLOOP_OUTPUT="$VENTOY_MODLOOP"',
            worker,
        )
        self.assertIn('OSTADIX_HOSTED_LIVE_ROOTFS="$ROOTFS_IMAGE"', worker)
        self.assertIn(
            'OSTADIX_HOSTED_LIVE_VENTOY_MODLOOP="$VENTOY_MODLOOP"', worker
        )
        self.assertIn('"rootfs": rootfs_identity', worker)
        self.assertIn('"ventoy_modloop": ventoy_modloop_identity', worker)
        self.assertIn('("serial", serial_smoke)', worker)
        self.assertIn('("graphical", visual_smoke)', worker)
        self.assertIn('("O-core", ocore_smoke)', worker)
        self.assertIn("foreign_kernel_lab.py", worker)
        self.assertIn('fetch "${GUEST_ARGUMENTS[@]}"', worker)
        self.assertIn('verify "${GUEST_ARGUMENTS[@]}"', worker)
        self.assertIn("OSTADIX_CAPACITY_HOST_KERNEL_FLAVOR=virt", worker)
        self.assertIn(
            'OSTADIX_HOSTED_LIVE_CAPACITY_HOST_INITRAMFS="$CAPACITY_HOST_INITRAMFS"',
            worker,
        )
        self.assertIn('"foreign_entries_nested_qemu_tcg": True', worker)
        self.assertIn('"foreign_entries_direct_grub": False', worker)
        self.assertIn('"foreign_guest_gui_proof": False', worker)
        self.assertIn('"foreign_guest_package_manager_execution_proof": False', worker)
        self.assertIn('"ventoy_foreign_route_proof": False', worker)

    def test_busybox_bootstrap_uses_supported_blkid_form(self) -> None:
        bootstrap = (
            ROOT / "scripts/prepare-x86_64-capacity-host.sh"
        ).read_text(encoding="utf-8")
        blkid_lines = [
            line.strip()
            for line in bootstrap.splitlines()
            if '"$BB" blkid' in line
        ]
        self.assertEqual(
            blkid_lines,
            [
                'block_identity=" $("$BB" blkid "$device" '
                '2>/dev/null || true) "'
            ],
        )
        self.assertNotIn(" -s ", blkid_lines[0])
        self.assertNotIn(" -o ", blkid_lines[0])

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
            RELEASE.MIN_GRAPHICAL_CHROMATIC_PIXELS,
            VGA_SMOKE.MIN_CHROMATIC_PIXELS,
        )
        self.assertEqual(
            RELEASE.MIN_GRAPHICAL_CHROMATIC_HUE_BUCKETS,
            VGA_SMOKE.MIN_CHROMATIC_HUE_BUCKETS,
        )
        self.assertEqual(
            RELEASE.MIN_GRAPHICAL_PIXELS_PER_HUE_BUCKET,
            VGA_SMOKE.MIN_PIXELS_PER_HUE_BUCKET,
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

            palette = (
                b"\xff\x30\x30",
                b"\xff\xd7\x30",
                b"\x50\xd0\x70",
                b"\x30\xd7\xd7",
                b"\x50\x80\xff",
                b"\xe0\x50\xd0",
                b"\xcd\xd6\xf4",
                b"\x1e\x1e\x2e",
            )
            pixels = bytearray(
                b"".join(palette[pixel % len(palette)] for pixel in range(width * height))
            )
            before_path = root / "before.ppm"
            before_path.write_bytes(b"P6\n320 200\n255\n" + pixels)
            before = VGA_SMOKE.read_frame(before_path)
            self.assertEqual(before.unique_colors, len(palette))
            self.assertGreaterEqual(
                before.chromatic_hue_buckets,
                VGA_SMOKE.MIN_CHROMATIC_HUE_BUCKETS,
            )
            VGA_SMOKE.validate_visible_frame(before)
            with self.assertRaisesRegex(VGA_SMOKE.VisualSmokeError, "did not visibly react"):
                VGA_SMOKE.changed_pixel_count(before, before)

            changed = bytearray(pixels)
            for pixel in range(400, 700):
                start = pixel * 3
                changed[start : start + 3] = bytes(
                    255 - channel for channel in changed[start : start + 3]
                )
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
        monitor.deadline = time.monotonic() + 1.0
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
                print("OSTADIX ISO IDENTITY bytes=7 sha256={'4' * 64}", flush=True)
                markers = {_complete_hosted_markers(SMOKE.REQUIRED_MARKERS)!r}
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
            self.assertEqual(result.public()["iso"], {"bytes": 7, "sha256": "4" * 64})
            self.assertEqual(result.public()["rootfs"], FIXTURE_ROOTFS_IDENTITY)
            self.assertEqual(result.public()["entropy"], FIXTURE_ENTROPY_EVIDENCE)
            self.assertEqual(result.public()["olangc_wasm"], FIXTURE_WASM_EVIDENCE)
            self.assertIn(SMOKE.REQUIRED_MARKERS[-1], output.getvalue())

    def test_marker_gate_rejects_duplicate_iso_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            script = self.fake_program(
                Path(directory),
                f"""
                import sys
                identity = "OSTADIX ISO IDENTITY bytes=7 sha256={'4' * 64}"
                print(identity, flush=True)
                print(identity, flush=True)
                markers = {_complete_hosted_markers(SMOKE.REQUIRED_MARKERS)!r}
                for marker in markers:
                    sys.stdout.buffer.write(marker + b"\\n")
                    sys.stdout.buffer.flush()
                sys.stdin.buffer.read(2)
                """,
            )
            with self.assertRaisesRegex(SMOKE.SmokeError, "exactly one"):
                SMOKE.run_marker_gate(
                    [sys.executable, str(script)], timeout_seconds=2
                )

    def test_marker_gate_rejects_malformed_or_duplicate_rootfs_identity(self) -> None:
        cases = []
        malformed = _complete_hosted_markers(SMOKE.REQUIRED_MARKERS)
        malformed[0] = b"OSTADIX HOSTED ROOTFS: PASS bytes=0 sha256=" + b"4" * 64
        cases.append((malformed, "zero-byte"))
        complete = _complete_hosted_markers(SMOKE.REQUIRED_MARKERS)
        cases.append(([complete[0], *complete], "duplicate"))

        for markers, label in cases:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                script = self.fake_program(
                    Path(directory),
                    f"""
                    import sys
                    print("OSTADIX ISO IDENTITY bytes=7 sha256={'4' * 64}", flush=True)
                    markers = {markers!r}
                    for marker in markers:
                        sys.stdout.buffer.write(marker + b"\\n")
                        sys.stdout.buffer.flush()
                    sys.stdin.buffer.read(2)
                    """,
                )
                with self.assertRaisesRegex(SMOKE.SmokeError, "exactly one full"):
                    SMOKE.run_marker_gate(
                        [sys.executable, str(script)], timeout_seconds=2
                    )

    def test_entropy_parser_requires_bound_qemu_device_probe_and_strength(self) -> None:
        marker = (
            b"OSTADIX HOSTED ENTROPY: PASS device=virtio-rng-pci "
            b"crng_bytes=32 available=256"
        )
        self.assertEqual(SMOKE._parse_entropy_identity(marker + b"\r\n"), 256)
        for transcript in (
            marker + b"\n" + marker + b"\n",
            b"OSTADIX HOSTED ENTROPY: PASS device=platform "
            b"crng_bytes=32 available=256\n",
            b"OSTADIX HOSTED ENTROPY: PASS device=virtio-rng-pci "
            b"crng_bytes=31 available=256\n",
            b"OSTADIX HOSTED ENTROPY: PASS device=virtio-rng-pci "
            b"crng_bytes=32 available=127\n",
        ):
            with self.subTest(transcript=transcript), self.assertRaises(
                SMOKE.SmokeError
            ):
                SMOKE._parse_entropy_identity(transcript)

    def test_marker_gate_rejects_displaced_entropy_with_a_generic_decoy(self) -> None:
        entropy_index = SMOKE.REQUIRED_MARKERS.index(SMOKE.ENTROPY_ORDERED_MARKER)
        for placement in ("before-rootfs", "after-node"):
            markers = _complete_hosted_markers(SMOKE.REQUIRED_MARKERS)
            entropy = markers.pop(entropy_index)
            markers.insert(entropy_index, SMOKE.ENTROPY_ORDERED_MARKER)
            markers.insert(0 if placement == "before-rootfs" else entropy_index + 2, entropy)
            with self.subTest(placement=placement), tempfile.TemporaryDirectory() as directory:
                script = self.fake_program(
                    Path(directory),
                    f"""
                    import sys
                    print("OSTADIX ISO IDENTITY bytes=7 sha256={'4' * 64}", flush=True)
                    for marker in {markers!r}:
                        sys.stdout.buffer.write(marker + b"\\n")
                        sys.stdout.buffer.flush()
                    sys.stdin.buffer.read(2)
                    """,
                )
                with self.assertRaisesRegex(
                    SMOKE.SmokeError,
                    "full Hosted entropy marker did not occupy its ordered position",
                ):
                    SMOKE.run_marker_gate(
                        [sys.executable, str(script)], timeout_seconds=0.2
                    )

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
