import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import struct
import subprocess
import sys
import tempfile
import tracemalloc
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "ostadix_capacity_iso.py"
SPEC = importlib.util.spec_from_file_location("ostadix_capacity_iso", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
ISO = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ISO
SPEC.loader.exec_module(ISO)


def _both16(value: int) -> bytes:
    return value.to_bytes(2, "little") + value.to_bytes(2, "big")


def _both32(value: int) -> bytes:
    return value.to_bytes(4, "little") + value.to_bytes(4, "big")


def _directory_record(
    name: bytes,
    lba: int,
    size: int,
    *,
    directory: bool,
    system_use: bytes = b"",
) -> bytes:
    base_length = 33 + len(name)
    if base_length & 1:
        base_length += 1
    length = base_length + len(system_use)
    if length & 1:
        length += 1
    if length > 255:
        raise AssertionError("synthetic ISO identifier is too long")
    record = bytearray(length)
    record[0] = length
    record[2:10] = _both32(lba)
    record[10:18] = _both32(size)
    record[18:25] = bytes((126, 1, 1, 0, 0, 0, 0))
    record[25] = 0x02 if directory else 0
    record[28:32] = _both16(1)
    record[32] = len(name)
    record[33 : 33 + len(name)] = name
    record[base_length : base_length + len(system_use)] = system_use
    return bytes(record)


def _rock_ridge_nm(name: str) -> bytes:
    payload = name.encode("utf-8")
    return b"NM" + bytes((5 + len(payload), 1, 0)) + payload


def _fat12_set(table: bytearray, cluster: int, value: int) -> None:
    offset = cluster + cluster // 2
    word = int.from_bytes(table[offset : offset + 2], "little")
    if cluster & 1:
        word = (word & 0x000F) | ((value & 0x0FFF) << 4)
    else:
        word = (word & 0xF000) | (value & 0x0FFF)
    table[offset : offset + 2] = word.to_bytes(2, "little")


def _fat_entry(name: bytes, suffix: bytes, attributes: int, cluster: int, size: int) -> bytes:
    record = bytearray(32)
    record[:8] = name.ljust(8, b" ")
    record[8:11] = suffix.ljust(3, b" ")
    record[11] = attributes
    record[26:28] = cluster.to_bytes(2, "little")
    record[28:32] = size.to_bytes(4, "little")
    return bytes(record)


def _efi_image() -> bytes:
    image = bytearray(8 * 512)
    image[0:3] = b"\xeb\x3c\x90"
    image[3:11] = b"MSDOS5.0"
    image[11:13] = (512).to_bytes(2, "little")
    image[13] = 1
    image[14:16] = (1).to_bytes(2, "little")
    image[16] = 1
    image[17:19] = (16).to_bytes(2, "little")
    image[19:21] = (8).to_bytes(2, "little")
    image[21] = 0xF8
    image[22:24] = (1).to_bytes(2, "little")
    image[24:26] = (32).to_bytes(2, "little")
    image[26:28] = (64).to_bytes(2, "little")
    image[510:512] = b"\x55\xaa"
    fat = bytearray(512)
    fat[:3] = b"\xf8\xff\xff"
    _fat12_set(fat, 2, 0xFFF)
    _fat12_set(fat, 3, 0xFFF)
    _fat12_set(fat, 4, 5)
    _fat12_set(fat, 5, 0xFFF)
    image[512:1024] = fat
    image[1024:1056] = _fat_entry(b"EFI", b"", 0x10, 2, 0)
    image[1536:1568] = _fat_entry(b"BOOT", b"", 0x10, 3, 0)

    bootloader = bytearray(1024)
    bootloader[:2] = b"MZ"
    bootloader[0x3C:0x40] = (0x80).to_bytes(4, "little")
    bootloader[0x80:0x84] = b"PE\0\0"
    bootloader[0x84:0x86] = (0x8664).to_bytes(2, "little")
    bootloader[0x86:0x88] = (1).to_bytes(2, "little")
    bootloader[0x94:0x96] = (0xF0).to_bytes(2, "little")
    bootloader[0x96:0x98] = (0x0022).to_bytes(2, "little")
    bootloader[0x98:0x9A] = (0x20B).to_bytes(2, "little")
    bootloader[0x9C:0xA0] = (0x200).to_bytes(4, "little")
    bootloader[0xA8:0xAC] = (0x1000).to_bytes(4, "little")
    bootloader[0xAC:0xB0] = (0x1000).to_bytes(4, "little")
    bootloader[0xB0:0xB8] = (0x400000).to_bytes(8, "little")
    bootloader[0xB8:0xBC] = (0x1000).to_bytes(4, "little")
    bootloader[0xBC:0xC0] = (0x200).to_bytes(4, "little")
    bootloader[0xD0:0xD4] = (0x2000).to_bytes(4, "little")
    bootloader[0xD4:0xD8] = (0x200).to_bytes(4, "little")
    bootloader[0xDC:0xDE] = (10).to_bytes(2, "little")
    bootloader[0x104:0x108] = (16).to_bytes(4, "little")
    section = 0x98 + 0xF0
    bootloader[section : section + 8] = b".text\0\0\0"
    bootloader[section + 8 : section + 12] = (0x100).to_bytes(4, "little")
    bootloader[section + 12 : section + 16] = (0x1000).to_bytes(4, "little")
    bootloader[section + 16 : section + 20] = (0x200).to_bytes(4, "little")
    bootloader[section + 20 : section + 24] = (0x200).to_bytes(4, "little")
    bootloader[section + 36 : section + 40] = (0x60000020).to_bytes(4, "little")
    bootloader[0x200] = 0xC3
    image[2048:2080] = _fat_entry(b"BOOTX64", b"EFI", 0x20, 4, len(bootloader))
    image[2560:3072] = bootloader[:512]
    image[3072:3584] = bootloader[512:]
    return bytes(image)


def _elf64_x86_64() -> bytes:
    image = bytearray(512)
    image[:4] = b"\x7fELF"
    image[4:7] = b"\x02\x01\x01"
    image[16:18] = (2).to_bytes(2, "little")
    image[18:20] = (62).to_bytes(2, "little")
    image[20:24] = (1).to_bytes(4, "little")
    image[24:32] = (0x100180).to_bytes(8, "little")
    image[32:40] = (64).to_bytes(8, "little")
    image[52:54] = (64).to_bytes(2, "little")
    image[54:56] = (56).to_bytes(2, "little")
    image[56:58] = (1).to_bytes(2, "little")
    program = 64
    image[program : program + 4] = (1).to_bytes(4, "little")
    image[program + 4 : program + 8] = (5).to_bytes(4, "little")
    image[program + 8 : program + 16] = (0).to_bytes(8, "little")
    image[program + 16 : program + 24] = (0x100000).to_bytes(8, "little")
    image[program + 24 : program + 32] = (0x100000).to_bytes(8, "little")
    image[program + 32 : program + 40] = len(image).to_bytes(8, "little")
    image[program + 40 : program + 48] = len(image).to_bytes(8, "little")
    image[program + 48 : program + 56] = (0x1000).to_bytes(8, "little")
    multiboot = 0x100
    header_length = 24
    magic = 0xE85250D6
    checksum = (-(magic + header_length)) & 0xFFFFFFFF
    image[multiboot : multiboot + 4] = magic.to_bytes(4, "little")
    image[multiboot + 8 : multiboot + 12] = header_length.to_bytes(4, "little")
    image[multiboot + 12 : multiboot + 16] = checksum.to_bytes(4, "little")
    image[multiboot + 20 : multiboot + 24] = (8).to_bytes(4, "little")
    image[0x180] = 0xF4
    return bytes(image)


def _linux_bzimage() -> bytes:
    image = bytearray(4096)
    image[0x1F1] = 4
    image[0x1F4:0x1F8] = (128).to_bytes(4, "little")
    image[0x1FE:0x200] = b"\x55\xaa"
    image[0x202:0x206] = b"HdrS"
    image[0x206:0x208] = (0x020F).to_bytes(2, "little")
    image[0x211] = 1
    image[0x214:0x218] = (0x100000).to_bytes(4, "little")
    image[0x236:0x238] = (1).to_bytes(2, "little")
    image[2560:] = b"K" * (len(image) - 2560)
    return bytes(image)


def _qcow2() -> bytes:
    image = bytearray(4096)
    image[:4] = b"QFI\xfb"
    image[4:8] = (3).to_bytes(4, "big")
    image[20:24] = (16).to_bytes(4, "big")
    image[24:32] = (512 * 1024 * 1024).to_bytes(8, "big")
    image[100:104] = (104).to_bytes(4, "big")
    return bytes(image)


def _raw_cd() -> bytes:
    image = bytearray(18 * ISO.LOGICAL_BLOCK_SIZE)
    descriptor = 16 * ISO.LOGICAL_BLOCK_SIZE
    image[descriptor] = 1
    image[descriptor + 1 : descriptor + 6] = b"CD001"
    image[descriptor + 6] = 1
    terminator = 17 * ISO.LOGICAL_BLOCK_SIZE
    image[terminator] = 255
    image[terminator + 1 : terminator + 6] = b"CD001"
    image[terminator + 6] = 1
    return bytes(image)


def _catalog(boot_image_lba: int) -> bytes:
    catalog = bytearray(ISO.LOGICAL_BLOCK_SIZE)
    validation = bytearray(32)
    validation[0] = 1
    validation[1] = ISO.EFI_PLATFORM_ID
    validation[4:28] = b"OSTADIX CAPACITY UEFI".ljust(24, b" ")
    validation[30:32] = b"\x55\xaa"
    validation[28:30] = ((-sum(struct.unpack("<16H", validation))) & 0xFFFF).to_bytes(
        2, "little"
    )
    catalog[:32] = validation
    catalog[32] = 0x88
    catalog[33] = ISO.NO_EMULATION_MEDIA_TYPE
    catalog[38:40] = (8).to_bytes(2, "little")
    catalog[40:44] = boot_image_lba.to_bytes(4, "little")
    return bytes(catalog)


class _Tree:
    def __init__(self, name: str, parent: "_Tree | None" = None):
        self.name = name
        self.parent = parent
        self.directories: dict[str, _Tree] = {}
        self.files: dict[str, bytes] = {}
        self.lba = 0


def _fixture(files: dict[str, bytes], *, rock_ridge: bool = False) -> tuple[bytes, dict[str, int]]:
    root = _Tree("")
    for logical_path, content in files.items():
        if not logical_path.startswith("/") or not content:
            raise AssertionError("invalid synthetic ISO file")
        components = logical_path[1:].split("/")
        current = root
        for component in components[:-1]:
            current = current.directories.setdefault(component, _Tree(component, current))
        current.files[components[-1]] = content

    directories: list[_Tree] = []

    def collect(directory: _Tree) -> None:
        directories.append(directory)
        for child in sorted(directory.directories.values(), key=lambda value: value.name):
            collect(child)

    collect(root)
    next_lba = 30
    for directory in directories:
        directory.lba = next_lba
        next_lba += 1
    file_records: dict[str, tuple[int, int]] = {}
    offsets: dict[str, int] = {}

    def allocate(directory: _Tree, prefix: str) -> None:
        nonlocal next_lba
        for name, content in sorted(directory.files.items()):
            logical_path = f"{prefix}/{name}" if prefix else f"/{name}"
            file_records[logical_path] = (next_lba, len(content))
            offsets[logical_path] = next_lba * ISO.LOGICAL_BLOCK_SIZE
            next_lba += (len(content) + ISO.LOGICAL_BLOCK_SIZE - 1) // ISO.LOGICAL_BLOCK_SIZE
        for name, child in sorted(directory.directories.items()):
            allocate(child, f"{prefix}/{name}" if prefix else f"/{name}")

    allocate(root, "")
    blocks = next_lba + 2
    image = bytearray(blocks * ISO.LOGICAL_BLOCK_SIZE)
    root_record = _directory_record(b"\x00", root.lba, ISO.LOGICAL_BLOCK_SIZE, directory=True)
    pvd = bytearray(ISO.LOGICAL_BLOCK_SIZE)
    pvd[0] = 1
    pvd[1:6] = b"CD001"
    pvd[6] = 1
    pvd[40:72] = ISO.VOLUME_ID.encode("ascii").ljust(32, b" ")
    pvd[80:88] = _both32(blocks)
    pvd[120:124] = _both16(1)
    pvd[124:128] = _both16(1)
    pvd[128:132] = _both16(ISO.LOGICAL_BLOCK_SIZE)
    pvd[132:140] = _both32(10)
    pvd[156 : 156 + len(root_record)] = root_record
    image[16 * ISO.LOGICAL_BLOCK_SIZE : 17 * ISO.LOGICAL_BLOCK_SIZE] = pvd
    boot_record = bytearray(ISO.LOGICAL_BLOCK_SIZE)
    boot_record[0] = 0
    boot_record[1:6] = b"CD001"
    boot_record[6] = 1
    boot_record[7:39] = ISO.EL_TORITO_SYSTEM_ID.ljust(32, b"\0")
    boot_record[71:75] = (20).to_bytes(4, "little")
    image[17 * ISO.LOGICAL_BLOCK_SIZE : 18 * ISO.LOGICAL_BLOCK_SIZE] = boot_record
    terminator = bytearray(ISO.LOGICAL_BLOCK_SIZE)
    terminator[0] = 255
    terminator[1:6] = b"CD001"
    terminator[6] = 1
    image[18 * ISO.LOGICAL_BLOCK_SIZE : 19 * ISO.LOGICAL_BLOCK_SIZE] = terminator
    image[20 * ISO.LOGICAL_BLOCK_SIZE : 21 * ISO.LOGICAL_BLOCK_SIZE] = _catalog(21)
    efi = _efi_image()
    image[21 * ISO.LOGICAL_BLOCK_SIZE : 21 * ISO.LOGICAL_BLOCK_SIZE + len(efi)] = efi

    def populate(directory: _Tree, prefix: str) -> None:
        parent = directory.parent or directory
        records = [
            _directory_record(b"\x00", directory.lba, ISO.LOGICAL_BLOCK_SIZE, directory=True),
            _directory_record(b"\x01", parent.lba, ISO.LOGICAL_BLOCK_SIZE, directory=True),
        ]
        for index, (name, child) in enumerate(sorted(directory.directories.items())):
            primary_name = f"D{index:07d}" if rock_ridge else name.upper()
            records.append(
                _directory_record(
                    primary_name.encode("ascii"),
                    child.lba,
                    ISO.LOGICAL_BLOCK_SIZE,
                    directory=True,
                    system_use=_rock_ridge_nm(name) if rock_ridge else b"",
                )
            )
        for index, (name, content) in enumerate(sorted(directory.files.items())):
            logical_path = f"{prefix}/{name}" if prefix else f"/{name}"
            lba, size = file_records[logical_path]
            primary_name = f"F{index:07d}.DAT" if rock_ridge else name.upper()
            records.append(
                _directory_record(
                    primary_name.encode("ascii") + b";1",
                    lba,
                    size,
                    directory=False,
                    system_use=_rock_ridge_nm(name) if rock_ridge else b"",
                )
            )
            start = lba * ISO.LOGICAL_BLOCK_SIZE
            image[start : start + len(content)] = content
        data = b"".join(records)
        if len(data) > ISO.LOGICAL_BLOCK_SIZE:
            raise AssertionError(f"synthetic directory too large: {prefix or '/'}")
        start = directory.lba * ISO.LOGICAL_BLOCK_SIZE
        image[start : start + ISO.LOGICAL_BLOCK_SIZE] = data.ljust(ISO.LOGICAL_BLOCK_SIZE, b"\0")
        for name, child in sorted(directory.directories.items()):
            populate(child, f"{prefix}/{name}" if prefix else f"/{name}")

    populate(root, "")
    return bytes(image), offsets


def _profile() -> dict[str, object]:
    artifacts = [
        ("/boot/entry/ostadix/kernel.elf", "ocore-kernel"),
        ("/boot/capacity-host/vmlinuz", "linux-kernel"),
        ("/boot/capacity-host/initrd", "linux-initrd"),
        ("/guests/direct/vmlinuz", "linux-kernel"),
        ("/guests/direct/initrd", "linux-initrd"),
        ("/guests/direct/rootfs.iso", "guest-rootfs"),
        ("/guests/plan9/disk.qcow2", "guest-qcow2"),
        ("/guests/redox/live.iso", "guest-raw-cd"),
    ]
    return {
        "schema": ISO.PROFILE_SCHEMA,
        "architecture": ISO.ARCHITECTURE,
        "default_entry": "ostadix",
        "artifacts": [
            {"iso_path": path, "stage_path": path[1:], "role": role} for path, role in artifacts
        ],
        "entries": [
            {
                "id": "ostadix",
                "title": "OSTADIX O-core [direct]",
                "hotkey": "o",
                "adapter": "multiboot2",
                "arguments": [],
                "kernel_path": "/boot/entry/ostadix/kernel.elf",
            },
            {
                "id": "linux",
                "title": "Linux [direct]",
                "hotkey": "l",
                "adapter": "linux",
                "arguments": ["console=ttyS0,115200n8"],
                "kernel_path": "/boot/capacity-host/vmlinuz",
                "initrd_paths": ["/boot/capacity-host/initrd"],
            },
            {
                "id": "guix",
                "title": "Guix [virtualized/TCG]",
                "hotkey": "g",
                "adapter": "qemu-tcg-linux-direct",
                "arguments": ["rdinit=/init"],
                "host_kernel_path": "/boot/capacity-host/vmlinuz",
                "host_initrd_path": "/boot/capacity-host/initrd",
                "selection_id": "guix-direct",
                "guest_artifact_paths": [
                    "/guests/direct/vmlinuz",
                    "/guests/direct/initrd",
                    "/guests/direct/rootfs.iso",
                ],
            },
            {
                "id": "plan9",
                "title": "Plan 9 [virtualized/TCG]",
                "hotkey": "p",
                "adapter": "qemu-tcg-qcow2",
                "arguments": ["rdinit=/init"],
                "host_kernel_path": "/boot/capacity-host/vmlinuz",
                "host_initrd_path": "/boot/capacity-host/initrd",
                "selection_id": "plan9-qcow2",
                "guest_artifact_paths": ["/guests/plan9/disk.qcow2"],
            },
            {
                "id": "redox",
                "title": "Redox [virtualized/TCG]",
                "hotkey": "r",
                "adapter": "qemu-tcg-raw-cd",
                "arguments": ["rdinit=/init"],
                "host_kernel_path": "/boot/capacity-host/vmlinuz",
                "host_initrd_path": "/boot/capacity-host/initrd",
                "selection_id": "redox-cd",
                "guest_artifact_paths": ["/guests/redox/live.iso"],
            },
            {
                "id": "redox-curses",
                "title": "Redox curses [virtualized/TCG]",
                "hotkey": "c",
                "adapter": "qemu-tcg-raw-cd-curses",
                "arguments": ["rdinit=/init"],
                "host_kernel_path": "/boot/capacity-host/vmlinuz",
                "host_initrd_path": "/boot/capacity-host/initrd",
                "selection_id": "redox-cd-curses",
                "guest_artifact_paths": ["/guests/redox/live.iso"],
            },
        ],
    }


def _artifact_bytes(*, bad_linux: bool = False) -> dict[str, bytes]:
    linux = bytearray(_linux_bzimage())
    if bad_linux:
        linux[0x202:0x206] = b"NOPE"
    return {
        "/boot/entry/ostadix/kernel.elf": _elf64_x86_64(),
        "/boot/capacity-host/vmlinuz": bytes(linux),
        "/boot/capacity-host/initrd": b"070701" + b"H" * 1024,
        "/guests/direct/vmlinuz": _linux_bzimage(),
        "/guests/direct/initrd": b"070701" + b"G" * 1024,
        "/guests/direct/rootfs.iso": _raw_cd(),
        "/guests/plan9/disk.qcow2": _qcow2(),
        "/guests/redox/live.iso": _raw_cd(),
    }


def _write_stage(stage: Path, artifacts: dict[str, bytes]) -> None:
    for path, content in artifacts.items():
        destination = stage / path[1:]
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(content)


def _capacity_fixture(
    temporary: Path,
    *,
    profile: dict[str, object] | None = None,
    artifacts: dict[str, bytes] | None = None,
    config_mutator=None,
    lock_mutator=None,
    rock_ridge: bool = False,
) -> tuple[bytes, dict[str, int], dict[str, object]]:
    stage = temporary / "stage"
    stage.mkdir(mode=0o700)
    artifacts = artifacts or _artifact_bytes()
    _write_stage(stage, artifacts)
    profile = copy.deepcopy(profile or _profile())
    profile_path = temporary / "profile.json"
    profile_path.write_text(json.dumps(profile), encoding="utf-8")
    lock = ISO.create_lock(stage, profile_path)
    config = (stage / "boot/grub/grub.cfg").read_bytes()
    lock_bytes = (stage / "ostadix/capacity.lock.json").read_bytes()
    if config_mutator is not None:
        config = config_mutator(config)
    if lock_mutator is not None:
        lock_bytes = lock_mutator(lock_bytes)
    files = dict(artifacts)
    files[ISO.GRUB_ISO_PATH] = config
    files[ISO.LOCK_ISO_PATH] = lock_bytes
    image, offsets = _fixture(files, rock_ridge=rock_ridge)
    return image, offsets, lock


class CapacityIsoTests(unittest.TestCase):
    def test_committed_toml_profile_matches_schema(self) -> None:
        parsed = ISO._load_profile(ROOT / "evidence" / "absorbed_capacity_iso.toml")
        self.assertEqual(len(parsed["artifacts"]), 10)
        self.assertEqual(len(parsed["entries"]), 7)
        self.assertEqual(parsed["default_entry"], "hosted")
        hosted = next(entry for entry in parsed["entries"] if entry["id"] == "hosted")
        self.assertEqual(hosted["adapter"], "linux-selection")
        self.assertEqual(hosted["selection_id"], "hosted")
        self.assertEqual(
            hosted["initrd_paths"], ["/boot/capacity-host/initramfs.cpio.gz"]
        )
        grub = ISO.render_grub(parsed["entries"], parsed["default_entry"]).decode("ascii")
        self.assertIn("set default='hosted'", grub)
        self.assertIn(
            "linux /boot/capacity-host/vmlinuz-virt ostadix.capacity=hosted",
            grub,
        )

    def test_create_lock_is_deterministic_and_renders_real_qemu_entries(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = root / "stage"
            stage.mkdir(mode=0o700)
            _write_stage(stage, _artifact_bytes())
            profile = root / "profile.json"
            profile.write_text(json.dumps(_profile(), indent=2), encoding="utf-8")
            first = ISO.create_lock(stage, profile)
            lock_bytes = (stage / "ostadix/capacity.lock.json").read_bytes()
            config = (stage / "boot/grub/grub.cfg").read_text(encoding="ascii")
            second = ISO.create_lock(stage, profile)
            self.assertEqual(first, second)
            self.assertEqual(lock_bytes, (stage / "ostadix/capacity.lock.json").read_bytes())
            self.assertEqual(lock_bytes, ISO.canonical_json(first))
            self.assertIn("linux /boot/capacity-host/vmlinuz ostadix.capacity=plan9-qcow2", config)
            self.assertIn("initrd /boot/capacity-host/initrd", config)
            self.assertIn("Plan 9 [virtualized/TCG]", config)
            self.assertIn(
                "serial --unit=0 --speed=115200 --word=8 --parity=no --stop=1\n"
                "terminal_input console serial\n"
                "terminal_output console serial\n",
                config,
            )
            self.assertEqual(stat.S_IMODE((stage / "ostadix/capacity.lock.json").stat().st_mode), 0o444)

    def test_json_and_toml_profile_paths_are_supported(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = root / "profile.json"
            profile.write_text(json.dumps(_profile()), encoding="utf-8")
            parsed = ISO._load_profile(profile)
            self.assertEqual(parsed["schema"], ISO.PROFILE_SCHEMA)
            self.assertEqual(ISO._load_profile(ROOT / "evidence/absorbed_capacity_iso.toml")["schema"], ISO.PROFILE_SCHEMA)

    def test_unknown_adapter_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            profile = _profile()
            profile["entries"][0]["adapter"] = "chainloader"
            path = Path(directory) / "profile.json"
            path.write_text(json.dumps(profile), encoding="utf-8")
            with self.assertRaisesRegex(ISO.CapacityIsoError, "adapter is unknown"):
                ISO._load_profile(path)

    def test_duplicate_artifact_path_and_hotkey_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            duplicate_path = _profile()
            duplicate_path["artifacts"][1]["iso_path"] = duplicate_path["artifacts"][0]["iso_path"]
            path = root / "duplicate-path.json"
            path.write_text(json.dumps(duplicate_path), encoding="utf-8")
            with self.assertRaisesRegex(ISO.CapacityIsoError, "duplicate profile artifact ISO path"):
                ISO._load_profile(path)
            duplicate_hotkey = _profile()
            duplicate_hotkey["entries"][1]["hotkey"] = duplicate_hotkey["entries"][0]["hotkey"]
            path = root / "duplicate-hotkey.json"
            path.write_text(json.dumps(duplicate_hotkey), encoding="utf-8")
            with self.assertRaisesRegex(ISO.CapacityIsoError, "duplicate entry hotkey"):
                ISO._load_profile(path)

    def test_qemu_entry_requires_virtualized_label_and_exact_host_roles(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = _profile()
            profile["entries"][2]["title"] = "Guix"
            path = root / "label.json"
            path.write_text(json.dumps(profile), encoding="utf-8")
            with self.assertRaisesRegex(ISO.CapacityIsoError, "must explicitly contain"):
                ISO._load_profile(path)
            profile = _profile()
            profile["entries"][2]["host_initrd_path"] = "/guests/plan9/disk.qcow2"
            path = root / "host-role.json"
            path.write_text(json.dumps(profile), encoding="utf-8")
            with self.assertRaisesRegex(ISO.CapacityIsoError, "expected one of"):
                ISO._load_profile(path)

    def test_private_stage_rejects_symlink_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = root / "stage"
            stage.mkdir(mode=0o700)
            artifacts = _artifact_bytes()
            _write_stage(stage, artifacts)
            victim = stage / "boot/entry/ostadix/kernel.elf"
            victim.unlink()
            victim.symlink_to(stage / "boot/capacity-host/vmlinuz")
            profile = root / "profile.json"
            profile.write_text(json.dumps(_profile()), encoding="utf-8")
            with self.assertRaisesRegex(ISO.CapacityIsoError, "cannot pin stage artifact"):
                ISO.create_lock(stage, profile)

    def test_inspect_valid_capacity_iso(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image, _offsets, lock = _capacity_fixture(root)
            path = root / "capacity.iso"
            path.write_bytes(image)
            metadata = ISO.inspect_path(path)
            self.assertEqual(metadata["schema"], ISO.INSPECT_SCHEMA)
            self.assertEqual(metadata["volume_id"], ISO.VOLUME_ID)
            self.assertEqual(metadata["sha256"], hashlib.sha256(image).hexdigest())
            self.assertEqual(metadata["entries"], lock["entries"])
            self.assertEqual(len(metadata["artifacts"]), 8)
            self.assertEqual(metadata["efi_bootloader_path"], "/EFI/BOOT/BOOTX64.EFI")

    def test_inspect_uses_rock_ridge_names_when_primary_names_are_mangled(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image, _offsets, lock = _capacity_fixture(root, rock_ridge=True)
            path = root / "rock-ridge-capacity.iso"
            path.write_bytes(image)
            metadata = ISO.inspect_path(path)
            self.assertEqual(metadata["entries"], lock["entries"])
            self.assertEqual(metadata["capacity_lock_path"], ISO.LOCK_ISO_PATH)

    def test_payload_tampering_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image, offsets, _lock = _capacity_fixture(root)
            mutable = bytearray(image)
            mutable[offsets["/guests/plan9/disk.qcow2"] + 200] ^= 1
            path = root / "tampered.iso"
            path.write_bytes(mutable)
            with self.assertRaisesRegex(ISO.CapacityIsoError, "SHA-256 differs"):
                ISO.inspect_path(path)

    def test_bootx64_and_el_torito_tampering_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image, _offsets, _lock = _capacity_fixture(root)
            broken_efi = bytearray(image)
            broken_efi[21 * ISO.LOGICAL_BLOCK_SIZE + 2560] = 0
            path = root / "broken-efi.iso"
            path.write_bytes(broken_efi)
            with self.assertRaisesRegex(ISO.CapacityIsoError, "BOOTX64.EFI lacks"):
                ISO.inspect_path(path)
            broken_catalog = bytearray(image)
            broken_catalog[20 * ISO.LOGICAL_BLOCK_SIZE + 32] = 0
            path = root / "broken-catalog.iso"
            path.write_bytes(broken_catalog)
            with self.assertRaisesRegex(ISO.CapacityIsoError, "not bootable"):
                ISO.inspect_path(path)

    def test_config_drift_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def mutate(config: bytes) -> bytes:
                return config.replace(b"set timeout=10", b"set timeout=11", 1)

            image, _offsets, _lock = _capacity_fixture(root, config_mutator=mutate)
            path = root / "drift.iso"
            path.write_bytes(image)
            with self.assertRaisesRegex(ISO.CapacityIsoError, "GRUB config SHA-256"):
                ISO.inspect_path(path)

    def test_unknown_lock_field_and_noncanonical_lock_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def unknown(raw: bytes) -> bytes:
                value = json.loads(raw)
                value["unexpected"] = True
                return ISO.canonical_json(value)

            image, _offsets, _lock = _capacity_fixture(root, lock_mutator=unknown)
            path = root / "unknown.iso"
            path.write_bytes(image)
            with self.assertRaisesRegex(ISO.CapacityIsoError, "unknown fields"):
                ISO.inspect_path(path)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image, _offsets, _lock = _capacity_fixture(root, lock_mutator=lambda raw: raw + b" ")
            path = root / "noncanonical.iso"
            path.write_bytes(image)
            with self.assertRaisesRegex(ISO.CapacityIsoError, "not exact canonical JSON"):
                ISO.inspect_path(path)

    def test_bad_linux_bzimage_is_rejected_even_when_locked(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image, _offsets, _lock = _capacity_fixture(root, artifacts=_artifact_bytes(bad_linux=True))
            path = root / "bad-linux.iso"
            path.write_bytes(image)
            with self.assertRaisesRegex(ISO.CapacityIsoError, "Linux boot protocol signature"):
                ISO.inspect_path(path)

    def test_atomic_publication_is_read_only_and_no_clobber(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image, _offsets, _lock = _capacity_fixture(root)
            source = root / "candidate.iso"
            output = root / "published.iso"
            source.write_bytes(image)
            metadata = ISO.publish_path(source, output)
            self.assertEqual(metadata, ISO.inspect_path(output, require_readonly=True))
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o444)
            with self.assertRaisesRegex(ISO.CapacityIsoError, "refusing to clobber"):
                ISO.publish_path(source, output)

    def test_multigib_sparse_inspection_has_bounded_reads_and_heap(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image, _offsets, _lock = _capacity_fixture(root)
            path = root / "sparse-capacity.iso"
            path.write_bytes(image)
            sparse_bytes = 2 * 1024 * 1024 * 1024
            with path.open("r+b", buffering=0) as stream:
                stream.truncate(sparse_bytes)
                stream.seek(16 * ISO.LOGICAL_BLOCK_SIZE + 80)
                stream.write(_both32(sparse_bytes // ISO.LOGICAL_BLOCK_SIZE))
            original_pread = os.pread
            maximum_request = 0

            def bounded_pread(descriptor: int, size: int, offset: int) -> bytes:
                nonlocal maximum_request
                maximum_request = max(maximum_request, size)
                return original_pread(descriptor, size, offset)

            tracemalloc.start()
            try:
                with mock.patch.object(ISO.os, "pread", side_effect=bounded_pread):
                    metadata = ISO.inspect_path(path)
                _current, peak = tracemalloc.get_traced_memory()
            finally:
                tracemalloc.stop()
            self.assertEqual(metadata["bytes"], sparse_bytes)
            self.assertLessEqual(maximum_request, ISO.STREAM_CHUNK_BYTES)
            self.assertLess(peak, 32 * 1024 * 1024)

    def test_builder_rejects_extra_outputs_before_preflight(self) -> None:
        builder = ROOT / "ocore/kernel/build-x86_64-capacity-iso.sh"
        builder_source = builder.read_text(encoding="utf-8")
        self.assertIn("export CARGO_NET_OFFLINE=true", builder_source)
        self.assertIn("ostadix-hosted-live-x86_64-uefi.iso", builder_source)
        with tempfile.TemporaryDirectory() as directory:
            build_root = Path(directory) / "must-not-exist"
            environment = os.environ.copy()
            environment["OSTADIX_CAPACITY_ISO_ROOT"] = str(build_root)
            result = subprocess.run(
                [str(builder), "first.iso", "second.iso"],
                text=True,
                capture_output=True,
                env=environment,
                timeout=30,
                check=False,
            )
            self.assertEqual(result.returncode, 2)
            self.assertIn(
                "Usage: build-x86_64-capacity-iso.sh [OUTPUT]",
                result.stderr,
            )
            self.assertFalse(build_root.exists())

    def test_ocore_builder_resolves_an_external_cargo_target_directory(self) -> None:
        source = (ROOT / "ocore/kernel/build.sh").read_text(encoding="utf-8")
        self.assertIn('OCOREC_BIN="$CARGO_TARGET_DIR/debug/ocorec"', source)
        self.assertIn('OCOREC_BIN="$(pwd -P)/$CARGO_TARGET_DIR/debug/ocorec"', source)
        self.assertIn('"$OCOREC_BIN" \\', source)

    def test_interactive_runner_preserves_standard_input_through_qemu_exec(self) -> None:
        runner = ROOT / "ocore" / "kernel" / "run-x86_64-capacity-iso-qemu.sh"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            media = root / "capacity.iso"
            firmware = root / "OVMF.fd"
            inspector = root / "fake_inspector.py"
            qemu = root / "fake-qemu"
            media.write_bytes(b"capacity")
            media.chmod(0o444)
            firmware.write_bytes(b"firmware")
            inspector.write_text(
                """#!/usr/bin/env python3
import os

def _open_pinned_regular(path, label, readonly=False):
    return os.open(path, os.O_RDONLY)

def inspect_descriptor(descriptor, label):
    return {"entries": [{"hotkey": "o", "title": "O-core", "adapter": "multiboot2"}]}
""",
                encoding="utf-8",
            )
            inspector.chmod(0o755)
            qemu.write_text(
                """#!/usr/bin/env python3
import sys
print("FAKE_QEMU_STDIN=" + sys.stdin.readline().rstrip("\\n"))
""",
                encoding="utf-8",
            )
            qemu.chmod(0o755)
            environment = os.environ.copy()
            environment.update(
                {
                    "OCORE_QEMU_BIN": str(qemu),
                    "OSTADIX_CAPACITY_ISO_INSPECTOR": str(inspector),
                    "OSTADIX_OVMF_CODE": str(firmware),
                    "OSTADIX_PYTHON": sys.executable,
                }
            )
            result = subprocess.run(
                [str(runner), str(media)],
                input="preserved-terminal-input\n",
                text=True,
                capture_output=True,
                env=environment,
                timeout=30,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("FAKE_QEMU_STDIN=preserved-terminal-input", result.stdout)

    def test_capacity_host_pins_optical_driver_modloop_and_module_namespace(self) -> None:
        source = (ROOT / "scripts" / "prepare-x86_64-capacity-host.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn("ALPINE_MODLOOP_BYTES=22867968", source)
        self.assertIn(
            "ALPINE_MODLOOP_SHA256=78907e7cc812d555f08d4e1133d090cf11fa197370882adfe67b0a5986ccb3f9",
            source,
        )
        for module in ("cdrom.ko", "sr_mod.ko", "isofs.ko"):
            self.assertIn(module, source)
        self.assertIn('ln -s ../usr/lib/modules "$STAGE/lib/modules"', source)

    def test_capacity_host_embeds_and_self_tests_hosted_ostadix_before_media_mount(self) -> None:
        source = (ROOT / "scripts" / "prepare-x86_64-capacity-host.sh").read_text(
            encoding="utf-8"
        )
        package_lock = (ROOT / "evidence" / "hosted_live_apk_packages.txt").read_text(
            encoding="utf-8"
        )
        packages = [
            line
            for line in package_lock.splitlines()
            if line and not line.startswith("#")
        ]
        self.assertEqual(packages, sorted(set(packages)))
        for package in (
            "bash=5.3.9-r1",
            "python3=3.14.7-r1",
            "qemu-system-x86_64=11.0.1-r0",
            "qemu-ui-curses=11.0.1-r0",
            "sqlite=3.53.4-r0",
        ):
            self.assertIn(package, packages)
        self.assertIn("PACKAGE_SPECS", source)
        self.assertIn("resolved Alpine package closure differs", source)
        self.assertIn("nameserver 1.1.1.1", source)
        self.assertIn("for binary in O o-cli olangc o-link; do", source)
        self.assertIn(
            'install -m 0555 "$HOSTED_BIN_DIR/$binary" "$STAGE/usr/local/bin/$binary"',
            source,
        )
        for marker in (
            "OSTADIX HOSTED O SMOKE: PASS",
            "OSTADIX HOSTED BASH: PASS",
            "OSTADIX HOSTED SQLITE: PASS",
            "OSTADIX HOSTED OLANGC IR: PASS",
            "OSTADIX HOSTED O-CLI: PASS",
            "OSTADIX HOSTED O-LINK: PASS",
            "OSTADIX HOSTED LIVE READY",
        ):
            self.assertIn(marker, source)
        hosted_branch = source.index('if [ "$selected" = hosted ]; then')
        media_mount = source.index("media=", hosted_branch)
        self.assertLess(hosted_branch, media_mount)


if __name__ == "__main__":
    unittest.main()
