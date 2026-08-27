import importlib.util
import hashlib
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
MODULE_PATH = ROOT / "scripts" / "ostadix_boot_iso.py"
SPEC = importlib.util.spec_from_file_location("ostadix_boot_iso", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
ISO = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ISO
SPEC.loader.exec_module(ISO)
WRAPPER_PATH = ROOT / "scripts" / "ostadix_xorriso_reproducible.py"
WRAPPER_SPEC = importlib.util.spec_from_file_location(
    "ostadix_xorriso_reproducible", WRAPPER_PATH
)
assert WRAPPER_SPEC is not None and WRAPPER_SPEC.loader is not None
WRAPPER = importlib.util.module_from_spec(WRAPPER_SPEC)
sys.modules[WRAPPER_SPEC.name] = WRAPPER
WRAPPER_SPEC.loader.exec_module(WRAPPER)


def _both16(value: int) -> bytes:
    return value.to_bytes(2, "little") + value.to_bytes(2, "big")


def _both32(value: int) -> bytes:
    return value.to_bytes(4, "little") + value.to_bytes(4, "big")


def _directory_record(
    name: bytes, extent_lba: int, size: int, *, directory: bool
) -> bytes:
    length = 33 + len(name)
    if length & 1:
        length += 1
    record = bytearray(length)
    record[0] = length
    record[2:10] = _both32(extent_lba)
    record[10:18] = _both32(size)
    record[18:25] = bytes((80, 1, 1, 0, 0, 0, 0))
    record[25] = 0x02 if directory else 0
    record[28:32] = _both16(1)
    record[32] = len(name)
    record[33 : 33 + len(name)] = name
    return bytes(record)


def _directory(*records: bytes) -> bytes:
    content = b"".join(records)
    if len(content) > ISO.LOGICAL_BLOCK_SIZE:
        raise AssertionError("test directory exceeds one ISO block")
    return content.ljust(ISO.LOGICAL_BLOCK_SIZE, b"\0")


def _fat12_set(table: bytearray, cluster: int, value: int) -> None:
    offset = cluster + cluster // 2
    word = int.from_bytes(table[offset : offset + 2], "little")
    if cluster & 1:
        word = (word & 0x000F) | ((value & 0x0FFF) << 4)
    else:
        word = (word & 0xF000) | (value & 0x0FFF)
    table[offset : offset + 2] = word.to_bytes(2, "little")


def _fat_entry(name: bytes, extension: bytes, attributes: int, cluster: int, size: int) -> bytes:
    if len(name) > 8 or len(extension) > 3:
        raise AssertionError("invalid synthetic FAT short name")
    record = bytearray(32)
    record[0:8] = name.ljust(8, b" ")
    record[8:11] = extension.ljust(3, b" ")
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
    fat[0:3] = b"\xf8\xff\xff"
    _fat12_set(fat, 2, 0xFFF)
    _fat12_set(fat, 3, 0xFFF)
    _fat12_set(fat, 4, 5)
    _fat12_set(fat, 5, 0xFFF)
    image[512:1024] = fat
    image[1024:1056] = _fat_entry(b"EFI", b"", 0x10, 2, 0)
    image[1536:1568] = _fat_entry(b"BOOT", b"", 0x10, 3, 0)
    bootloader_mutable = bytearray(1024)
    bootloader_mutable[0:2] = b"MZ"
    bootloader_mutable[0x3C:0x40] = (0x80).to_bytes(4, "little")
    bootloader_mutable[0x80:0x84] = b"PE\0\0"
    bootloader_mutable[0x84:0x86] = (0x8664).to_bytes(2, "little")
    bootloader_mutable[0x86:0x88] = (1).to_bytes(2, "little")
    bootloader_mutable[0x94:0x96] = (0xF0).to_bytes(2, "little")
    bootloader_mutable[0x96:0x98] = (0x0022).to_bytes(2, "little")
    bootloader_mutable[0x98:0x9A] = (0x20B).to_bytes(2, "little")
    bootloader_mutable[0x98 + 4 : 0x98 + 8] = (0x200).to_bytes(4, "little")
    bootloader_mutable[0x98 + 16 : 0x98 + 20] = (0x1000).to_bytes(4, "little")
    bootloader_mutable[0x98 + 20 : 0x98 + 24] = (0x1000).to_bytes(4, "little")
    bootloader_mutable[0x98 + 24 : 0x98 + 32] = (0x400000).to_bytes(8, "little")
    bootloader_mutable[0x98 + 32 : 0x98 + 36] = (0x1000).to_bytes(4, "little")
    bootloader_mutable[0x98 + 36 : 0x98 + 40] = (0x200).to_bytes(4, "little")
    bootloader_mutable[0x98 + 56 : 0x98 + 60] = (0x2000).to_bytes(4, "little")
    bootloader_mutable[0x98 + 60 : 0x98 + 64] = (0x200).to_bytes(4, "little")
    bootloader_mutable[0x98 + 68 : 0x98 + 70] = (10).to_bytes(2, "little")
    bootloader_mutable[0x98 + 108 : 0x98 + 112] = (16).to_bytes(4, "little")
    section_offset = 0x98 + 0xF0
    bootloader_mutable[section_offset : section_offset + 8] = b".text\0\0\0"
    bootloader_mutable[section_offset + 8 : section_offset + 12] = (0x100).to_bytes(
        4, "little"
    )
    bootloader_mutable[section_offset + 12 : section_offset + 16] = (0x1000).to_bytes(
        4, "little"
    )
    bootloader_mutable[section_offset + 16 : section_offset + 20] = (0x200).to_bytes(
        4, "little"
    )
    bootloader_mutable[section_offset + 20 : section_offset + 24] = (0x200).to_bytes(
        4, "little"
    )
    bootloader_mutable[section_offset + 36 : section_offset + 40] = (
        0x60000020
    ).to_bytes(4, "little")
    bootloader_mutable[0x200] = 0xC3
    bootloader = bytes(bootloader_mutable)
    image[2048:2080] = _fat_entry(b"BOOTX64", b"EFI", 0x20, 4, len(bootloader))
    image[2560 : 2560 + 512] = bootloader[:512]
    image[3072 : 3072 + len(bootloader) - 512] = bootloader[512:]
    return bytes(image)


def _elf64_x86_64() -> bytes:
    image = bytearray(512)
    image[0:4] = b"\x7fELF"
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


def _catalog(boot_image_lba: int) -> bytes:
    catalog = bytearray(ISO.LOGICAL_BLOCK_SIZE)
    validation = bytearray(32)
    validation[0] = 0x01
    validation[1] = ISO.EFI_PLATFORM_ID
    validation[4:28] = b"OSTADIX UEFI".ljust(24, b" ")
    validation[30:32] = b"\x55\xaa"
    checksum = (-sum(struct.unpack("<16H", validation))) & 0xFFFF
    validation[28:30] = checksum.to_bytes(2, "little")
    catalog[0:32] = validation
    catalog[32] = 0x88
    catalog[33] = ISO.NO_EMULATION_MEDIA_TYPE
    catalog[38:40] = (8).to_bytes(2, "little")
    catalog[40:44] = boot_image_lba.to_bytes(4, "little")
    return bytes(catalog)


def _fixture(config: bytes | None = None) -> bytes:
    blocks = 40
    boot_catalog_lba = 20
    boot_image_lba = 21
    root_lba = 30
    boot_lba = 31
    grub_lba = 32
    kernel_lba = 33
    config_lba = 34
    image = bytearray(blocks * ISO.LOGICAL_BLOCK_SIZE)
    kernel = _elf64_x86_64()
    if config is None:
        config = (ROOT / "ocore" / "kernel" / "x86_64" / "grub-iso.cfg").read_bytes()

    root_record = _directory_record(b"\x00", root_lba, ISO.LOGICAL_BLOCK_SIZE, directory=True)
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
    boot_record[71:75] = boot_catalog_lba.to_bytes(4, "little")
    image[17 * ISO.LOGICAL_BLOCK_SIZE : 18 * ISO.LOGICAL_BLOCK_SIZE] = boot_record

    terminator = bytearray(ISO.LOGICAL_BLOCK_SIZE)
    terminator[0] = 255
    terminator[1:6] = b"CD001"
    terminator[6] = 1
    image[18 * ISO.LOGICAL_BLOCK_SIZE : 19 * ISO.LOGICAL_BLOCK_SIZE] = terminator
    image[
        boot_catalog_lba * ISO.LOGICAL_BLOCK_SIZE : (boot_catalog_lba + 1)
        * ISO.LOGICAL_BLOCK_SIZE
    ] = _catalog(boot_image_lba)
    efi = _efi_image()
    image[
        boot_image_lba * ISO.LOGICAL_BLOCK_SIZE : boot_image_lba
        * ISO.LOGICAL_BLOCK_SIZE
        + len(efi)
    ] = efi

    image[root_lba * ISO.LOGICAL_BLOCK_SIZE : (root_lba + 1) * ISO.LOGICAL_BLOCK_SIZE] = _directory(
        root_record,
        _directory_record(b"\x01", root_lba, ISO.LOGICAL_BLOCK_SIZE, directory=True),
        _directory_record(b"BOOT", boot_lba, ISO.LOGICAL_BLOCK_SIZE, directory=True),
    )
    image[boot_lba * ISO.LOGICAL_BLOCK_SIZE : (boot_lba + 1) * ISO.LOGICAL_BLOCK_SIZE] = _directory(
        _directory_record(b"\x00", boot_lba, ISO.LOGICAL_BLOCK_SIZE, directory=True),
        _directory_record(b"\x01", root_lba, ISO.LOGICAL_BLOCK_SIZE, directory=True),
        _directory_record(b"GRUB", grub_lba, ISO.LOGICAL_BLOCK_SIZE, directory=True),
        _directory_record(b"KERNEL.ELF;1", kernel_lba, len(kernel), directory=False),
    )
    image[grub_lba * ISO.LOGICAL_BLOCK_SIZE : (grub_lba + 1) * ISO.LOGICAL_BLOCK_SIZE] = _directory(
        _directory_record(b"\x00", grub_lba, ISO.LOGICAL_BLOCK_SIZE, directory=True),
        _directory_record(b"\x01", boot_lba, ISO.LOGICAL_BLOCK_SIZE, directory=True),
        _directory_record(b"GRUB.CFG;1", config_lba, len(config), directory=False),
    )
    image[kernel_lba * ISO.LOGICAL_BLOCK_SIZE : kernel_lba * ISO.LOGICAL_BLOCK_SIZE + len(kernel)] = kernel
    image[config_lba * ISO.LOGICAL_BLOCK_SIZE : config_lba * ISO.LOGICAL_BLOCK_SIZE + len(config)] = config
    return bytes(image)


def _write_sparse_extended_fixture(path: Path, total_bytes: int) -> None:
    if total_bytes % ISO.LOGICAL_BLOCK_SIZE or total_bytes < len(_fixture()):
        raise AssertionError("invalid sparse ISO fixture size")
    image = bytearray(_fixture())
    volume_blocks = total_bytes // ISO.LOGICAL_BLOCK_SIZE
    primary_volume_size = 16 * ISO.LOGICAL_BLOCK_SIZE + 80
    image[primary_volume_size : primary_volume_size + 8] = _both32(volume_blocks)
    with path.open("wb") as stream:
        stream.write(image)
        stream.truncate(total_bytes)


def _grub_rescue_tree(
    root: Path, token: str, *, auxiliary_boot: str = "boot.efi"
) -> Path:
    tree = root / "grub-private"
    (tree / ".disk").mkdir(parents=True)
    (tree / "efi/boot").mkdir(parents=True)
    (tree / ".disk" / f"{token}.uuid").write_bytes(b"")
    payload = b"MZ" + token.encode("ascii") + b" fixed payload"
    (tree / "efi/boot/bootx64.efi").write_bytes(payload)
    auxiliary = tree / auxiliary_boot
    auxiliary.parent.mkdir(parents=True, exist_ok=True)
    auxiliary.write_bytes(payload)
    fat = bytearray(1024)
    fat[0:3] = b"\xeb\x3c\x90"
    fat[38] = 0x29
    fat[39:43] = b"RAND"
    fat[510:512] = b"\x55\xaa"
    fat[600 : 600 + len(payload)] = payload
    (tree / "efi.img").write_bytes(fat)
    return tree


class OstadixBootIsoTests(unittest.TestCase):
    def test_strict_fixture_reports_distinct_iso_contract(self) -> None:
        image = _fixture()
        first = ISO.inspect_image(image)
        second = ISO.inspect_image(image)
        self.assertEqual(first, second)
        self.assertEqual(first["schema"], "ostadix.boot-iso/v1")
        self.assertEqual(first["bytes"], len(image))
        self.assertEqual(first["volume_id"], "OSTADIX")
        self.assertEqual(first["el_torito_platform_id"], 0xEF)
        self.assertEqual(first["el_torito_media_type"], 0)
        self.assertEqual(first["efi_bootloader_path"], "/EFI/BOOT/BOOTX64.EFI")
        self.assertEqual(first["kernel_path"], "/boot/kernel.elf")
        self.assertEqual(first["grub_config_path"], "/boot/grub/grub.cfg")
        self.assertEqual(
            hashlib.sha256(
                (ROOT / "ocore/kernel/x86_64/grub-iso.cfg").read_bytes()
            ).hexdigest(),
            ISO.EXPECTED_GRUB_CONFIG_SHA256,
        )

    def test_rejects_emulated_uefi_boot_entry(self) -> None:
        image = bytearray(_fixture())
        image[20 * ISO.LOGICAL_BLOCK_SIZE + 33] = 1
        with self.assertRaisesRegex(ISO.IsoError, "not no-emulation"):
            ISO.inspect_image(bytes(image))

    def test_rejects_non_uefi_boot_catalog(self) -> None:
        image = bytearray(_fixture())
        validation_offset = 20 * ISO.LOGICAL_BLOCK_SIZE
        validation = bytearray(image[validation_offset : validation_offset + 32])
        validation[1] = 0
        validation[28:30] = b"\0\0"
        validation[28:30] = ((-sum(struct.unpack("<16H", validation))) & 0xFFFF).to_bytes(
            2, "little"
        )
        image[validation_offset : validation_offset + 32] = validation
        with self.assertRaisesRegex(ISO.IsoError, "0 UEFI entries"):
            ISO.inspect_image(bytes(image))

    def test_rejects_non_pe_uefi_bootloader(self) -> None:
        image = bytearray(_fixture())
        bootloader_offset = 21 * ISO.LOGICAL_BLOCK_SIZE + 5 * 512
        image[bootloader_offset : bootloader_offset + 2] = b"NO"
        with self.assertRaisesRegex(ISO.IsoError, "lacks a DOS/PE"):
            ISO.inspect_image(bytes(image))

    def test_rejects_wrong_uefi_pe_machine(self) -> None:
        image = bytearray(_fixture())
        bootloader_offset = 21 * ISO.LOGICAL_BLOCK_SIZE + 5 * 512
        image[bootloader_offset + 0x84 : bootloader_offset + 0x86] = (0x014C).to_bytes(
            2, "little"
        )
        with self.assertRaisesRegex(ISO.IsoError, "not an x86_64 PE"):
            ISO.inspect_image(bytes(image))

    def test_rejects_non_pe32_plus_uefi_bootloader(self) -> None:
        image = bytearray(_fixture())
        bootloader_offset = 21 * ISO.LOGICAL_BLOCK_SIZE + 5 * 512
        image[bootloader_offset + 0x98 : bootloader_offset + 0x9A] = (0x10B).to_bytes(
            2, "little"
        )
        with self.assertRaisesRegex(ISO.IsoError, r"not PE32\+"):
            ISO.inspect_image(bytes(image))

    def test_rejects_non_application_uefi_subsystem(self) -> None:
        image = bytearray(_fixture())
        bootloader_offset = 21 * ISO.LOGICAL_BLOCK_SIZE + 5 * 512
        subsystem = bootloader_offset + 0x98 + 68
        image[subsystem : subsystem + 2] = (11).to_bytes(2, "little")
        with self.assertRaisesRegex(ISO.IsoError, "not an EFI application"):
            ISO.inspect_image(bytes(image))

    def test_rejects_zero_uefi_entry_point(self) -> None:
        image = bytearray(_fixture())
        bootloader_offset = 21 * ISO.LOGICAL_BLOCK_SIZE + 5 * 512
        entry = bootloader_offset + 0x98 + 16
        image[entry : entry + 4] = bytes(4)
        with self.assertRaisesRegex(ISO.IsoError, "zero executable entry"):
            ISO.inspect_image(bytes(image))

    def test_rejects_uefi_entry_in_non_executable_section(self) -> None:
        image = bytearray(_fixture())
        bootloader_offset = 21 * ISO.LOGICAL_BLOCK_SIZE + 5 * 512
        section_characteristics = bootloader_offset + 0x98 + 0xF0 + 36
        image[section_characteristics : section_characteristics + 4] = (
            0x40000040
        ).to_bytes(4, "little")
        with self.assertRaisesRegex(ISO.IsoError, "file-backed executable section"):
            ISO.inspect_image(bytes(image))

    def test_rejects_wrong_kernel_architecture(self) -> None:
        image = bytearray(_fixture())
        kernel_offset = 33 * ISO.LOGICAL_BLOCK_SIZE
        image[kernel_offset + 18 : kernel_offset + 20] = (183).to_bytes(2, "little")
        with self.assertRaisesRegex(ISO.IsoError, "not x86_64"):
            ISO.inspect_image(bytes(image))

    def test_rejects_kernel_without_a_multiboot2_header(self) -> None:
        image = bytearray(_fixture())
        kernel_offset = 33 * ISO.LOGICAL_BLOCK_SIZE
        image[kernel_offset + 0x100 : kernel_offset + 0x104] = bytes(4)
        with self.assertRaisesRegex(ISO.IsoError, "0 valid Multiboot2 headers"):
            ISO.inspect_image(bytes(image))

    def test_rejects_kernel_without_a_pt_load_segment(self) -> None:
        image = bytearray(_fixture())
        kernel_offset = 33 * ISO.LOGICAL_BLOCK_SIZE
        image[kernel_offset + 64 : kernel_offset + 68] = bytes(4)
        with self.assertRaisesRegex(ISO.IsoError, "no PT_LOAD"):
            ISO.inspect_image(bytes(image))

    def test_rejects_kernel_entry_outside_executable_file_bytes(self) -> None:
        image = bytearray(_fixture())
        kernel_offset = 33 * ISO.LOGICAL_BLOCK_SIZE
        image[kernel_offset + 24 : kernel_offset + 32] = (0x200000).to_bytes(
            8, "little"
        )
        with self.assertRaisesRegex(ISO.IsoError, "file-backed executable code"):
            ISO.inspect_image(bytes(image))

    def test_rejects_disk_media_uuid_search_in_iso_config(self) -> None:
        config = b"search --fs-uuid --set=root DEAF-BEEF\nmultiboot2 /boot/kernel.elf\n"
        with self.assertRaisesRegex(ISO.IsoError, "must not search"):
            ISO.inspect_image(_fixture(config))

    def test_rejects_extra_grub_commands_around_valid_multiboot(self) -> None:
        config = (
            ROOT / "ocore" / "kernel" / "x86_64" / "grub-iso.cfg"
        ).read_bytes() + b"\necho unadmitted\n"
        with self.assertRaisesRegex(ISO.IsoError, "exact committed"):
            ISO.inspect_image(_fixture(config))

    def test_inspect_path_rejects_input_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            actual = root / "actual.iso"
            link = root / "link.iso"
            actual.write_bytes(_fixture())
            link.symlink_to(actual)
            with self.assertRaisesRegex(ISO.IsoError, "without following links"):
                ISO.inspect_path(link)

    def test_descriptor_inspection_preserves_the_legacy_v1_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "legacy.iso"
            payload = _fixture()
            image.write_bytes(payload)
            expected = ISO.inspect_image(payload)
            descriptor = os.open(image, os.O_RDONLY)
            try:
                self.assertEqual(
                    ISO.inspect_descriptor(descriptor, "legacy fixture"), expected
                )
            finally:
                os.close(descriptor)

    def test_inspect_path_accepts_sparse_image_over_legacy_one_gib_boundedly(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "large-sparse.iso"
            total_bytes = 1024 * 1024 * 1024 + ISO.LOGICAL_BLOCK_SIZE
            _write_sparse_extended_fixture(image, total_bytes)

            tracemalloc.start()
            try:
                metadata = ISO.inspect_path(image)
                _current, peak = tracemalloc.get_traced_memory()
            finally:
                tracemalloc.stop()

            self.assertEqual(metadata["schema"], "ostadix.boot-iso/v1")
            self.assertEqual(metadata["bytes"], total_bytes)
            self.assertEqual(
                metadata["volume_blocks"], total_bytes // ISO.LOGICAL_BLOCK_SIZE
            )
            self.assertLess(peak, 32 * 1024 * 1024)

    def test_inspect_path_rejects_sparse_image_over_sixteen_gib_before_mapping(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "too-large-sparse.iso"
            with image.open("wb") as stream:
                stream.truncate(ISO.MAX_ISO_BYTES + ISO.LOGICAL_BLOCK_SIZE)
            with mock.patch.object(
                ISO.mmap, "mmap", side_effect=AssertionError("oversized ISO was mapped")
            ):
                with self.assertRaisesRegex(ISO.IsoError, "size outside"):
                    ISO.inspect_path(image)
            descriptor = os.open(image, os.O_RDONLY)
            try:
                with mock.patch.object(
                    ISO.mmap,
                    "mmap",
                    side_effect=AssertionError("caller raised the hard ISO ceiling"),
                ):
                    with self.assertRaisesRegex(ISO.IsoError, "size outside"):
                        ISO.inspect_descriptor(
                            descriptor,
                            "oversized fixture",
                            maximum=2 * ISO.MAX_ISO_BYTES,
                        )
            finally:
                os.close(descriptor)

    def test_descriptor_inspection_detects_mutation_during_validation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "mutable.iso"
            image.write_bytes(_fixture())
            descriptor = os.open(image, os.O_RDONLY)
            real_inspect = ISO.inspect_image

            def inspect_then_grow(mapping: object) -> dict[str, object]:
                metadata = real_inspect(mapping)
                writer = os.open(image, os.O_WRONLY)
                try:
                    os.ftruncate(writer, image.stat().st_size + ISO.LOGICAL_BLOCK_SIZE)
                finally:
                    os.close(writer)
                return metadata

            try:
                with mock.patch.object(
                    ISO, "inspect_image", side_effect=inspect_then_grow
                ):
                    with self.assertRaisesRegex(ISO.IsoError, "changed while"):
                        ISO.inspect_descriptor(descriptor, "mutable fixture")
            finally:
                os.close(descriptor)

    def test_inspect_path_detects_replacement_after_descriptor_validation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "replaceable.iso"
            replacement = root / "replacement.iso"
            image.write_bytes(_fixture())
            replacement.write_bytes(_fixture())
            real_inspect_descriptor = ISO.inspect_descriptor

            def inspect_then_replace(*arguments: object, **keywords: object) -> object:
                metadata = real_inspect_descriptor(*arguments, **keywords)
                os.replace(replacement, image)
                return metadata

            with mock.patch.object(
                ISO, "inspect_descriptor", side_effect=inspect_then_replace
            ):
                with self.assertRaisesRegex(ISO.IsoError, "path was replaced"):
                    ISO.inspect_path(image)

    def test_publish_rejects_output_symlink_without_touching_target(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "candidate.iso"
            victim = root / "victim"
            output = root / "output.iso"
            source.write_bytes(_fixture())
            victim.write_bytes(b"preserve me")
            output.symlink_to(victim)
            with self.assertRaisesRegex(ISO.IsoError, "output symlink"):
                ISO.publish_path(source, output)
            self.assertEqual(victim.read_bytes(), b"preserve me")
            self.assertTrue(output.is_symlink())

    def test_publish_round_trip_is_byte_exact_and_regular(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "candidate.iso"
            output = root / "ostadix.iso"
            source.write_bytes(_fixture())
            metadata = ISO.publish_path(source, output)
            self.assertEqual(source.read_bytes(), output.read_bytes())
            self.assertEqual(metadata, ISO.inspect_path(output))
            self.assertTrue(stat.S_ISREG(os.stat(output, follow_symlinks=False).st_mode))
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o444)

    def test_publish_streams_bounded_chunks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "candidate.iso"
            output = root / "ostadix.iso"
            total_bytes = 2 * ISO.COPY_CHUNK_BYTES + ISO.LOGICAL_BLOCK_SIZE
            _write_sparse_extended_fixture(source, total_bytes)
            real_pread = ISO.os.pread
            requested_sizes: list[int] = []

            def bounded_pread(descriptor: int, size: int, offset: int) -> bytes:
                requested_sizes.append(size)
                return real_pread(descriptor, size, offset)

            with mock.patch.object(ISO.os, "pread", side_effect=bounded_pread):
                metadata = ISO.publish_path(source, output)

            self.assertEqual(metadata["bytes"], total_bytes)
            self.assertTrue(requested_sizes)
            self.assertLessEqual(max(requested_sizes), ISO.COPY_CHUNK_BYTES)
            self.assertIn(ISO.COPY_CHUNK_BYTES, requested_sizes)
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o444)

    def test_publish_rejects_source_replacement_without_clobbering_output(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "candidate.iso"
            replacement = root / "replacement.iso"
            output = root / "ostadix.iso"
            source.write_bytes(_fixture())
            replacement.write_bytes(_fixture())
            output.write_bytes(b"preserve existing output")
            real_stream_copy = ISO._stream_copy_descriptor

            def copy_then_replace(*arguments: object, **keywords: object) -> str:
                digest = real_stream_copy(*arguments, **keywords)
                os.replace(replacement, source)
                return digest

            with mock.patch.object(
                ISO, "_stream_copy_descriptor", side_effect=copy_then_replace
            ):
                with self.assertRaisesRegex(ISO.IsoError, "path was replaced"):
                    ISO.publish_path(source, output)

            self.assertEqual(output.read_bytes(), b"preserve existing output")
            self.assertEqual(list(root.glob(".ostadix-iso-publish.*.tmp")), [])

    def test_publish_never_uses_pathname_chmod(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "candidate.iso"
            output = root / "ostadix.iso"
            source.write_bytes(_fixture())
            with mock.patch.object(
                ISO.os, "chmod", side_effect=AssertionError("pathname chmod used")
            ):
                ISO.publish_path(source, output)
            self.assertEqual(source.read_bytes(), output.read_bytes())

    def test_pinned_descriptor_inspects_a_comma_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "ostadix,comma.iso"
            image.write_bytes(_fixture())
            descriptor = ISO._open_pinned_regular(image, nofollow=True)
            try:
                self.assertEqual(
                    ISO.inspect_descriptor(descriptor, str(image))["schema"], ISO.SCHEMA
                )
            finally:
                os.close(descriptor)

    def test_cli_inspect_emits_machine_readable_schema(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "ostadix.iso"
            image.write_bytes(_fixture())
            result = subprocess.run(
                [sys.executable, str(MODULE_PATH), "inspect", str(image)],
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertEqual(json.loads(result.stdout)["schema"], ISO.SCHEMA)

    def test_xorriso_wrapper_canonicalizes_grub_clock_and_fat_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = _grub_rescue_tree(root / "one", "2026-08-27-12-22-52-00")
            second = _grub_rescue_tree(root / "two", "2026-08-27-12-22-54-00")
            first_kernel = first / "boot/kernel.elf"
            second_kernel = second / "boot/kernel.elf"
            first_kernel.parent.mkdir()
            second_kernel.parent.mkdir()
            first_kernel.write_bytes(b"kernel 2026-08-27-12-22-52-00 payload")
            second_kernel.write_bytes(b"kernel 2026-08-27-12-22-54-00 payload")
            first_args = WRAPPER.canonicalize(
                ["-as", "mkisofs", "--modification-date=2026082712225200", str(first)],
                315532800,
            )
            second_args = WRAPPER.canonicalize(
                ["-as", "mkisofs", "--modification-date=2026082712225400", str(second)],
                315532800,
            )
            self.assertEqual(first_args[:-1], second_args[:-1])
            self.assertEqual(first_args[2], "--modification-date=1980010100000000")
            fixed = "1980-01-01-00-00-00-00"
            self.assertTrue((first / ".disk" / f"{fixed}.uuid").is_file())
            self.assertTrue((second / ".disk" / f"{fixed}.uuid").is_file())
            for relative in ("efi.img", "efi/boot/bootx64.efi", "boot.efi"):
                self.assertEqual((first / relative).read_bytes(), (second / relative).read_bytes())
            self.assertEqual(
                first_kernel.read_bytes(), b"kernel 2026-08-27-12-22-52-00 payload"
            )
            self.assertEqual(
                second_kernel.read_bytes(), b"kernel 2026-08-27-12-22-54-00 payload"
            )
            self.assertEqual(
                os.stat(first / "efi.img").st_mtime_ns, 315532800 * 1_000_000_000
            )

    def test_xorriso_wrapper_rejects_a_symlink_in_grub_private_tree(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tree = _grub_rescue_tree(root, "2026-08-27-12-22-52-00")
            (tree / "unsafe").symlink_to(tree / "boot.efi")
            with self.assertRaisesRegex(WRAPPER.CanonicalizationError, "unsafe"):
                WRAPPER.canonicalize(["-as", "mkisofs", str(tree)], 315532800)

    def test_xorriso_wrapper_admits_debian_auxiliary_boot_layout(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            token = "2026-08-27-12-22-52-00"
            relative = "System/Library/CoreServices/boot.efi"
            tree = _grub_rescue_tree(
                Path(directory), token, auxiliary_boot=relative
            )
            WRAPPER.canonicalize(["-as", "mkisofs", str(tree)], 315532800)
            content = (tree / relative).read_bytes()
            self.assertIn(b"1980-01-01-00-00-00-00", content)
            self.assertNotIn(token.encode("ascii"), content)

    def test_xorriso_wrapper_rejects_ambiguous_auxiliary_boot_layout(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            token = "2026-08-27-12-22-52-00"
            tree = _grub_rescue_tree(Path(directory), token)
            second = tree / "System/Library/CoreServices/boot.efi"
            second.parent.mkdir(parents=True)
            second.write_bytes((tree / "boot.efi").read_bytes())
            with self.assertRaisesRegex(
                WRAPPER.CanonicalizationError, "exactly one admitted"
            ):
                WRAPPER.canonicalize(["-as", "mkisofs", str(tree)], 315532800)

    def test_xorriso_wrapper_requires_one_token_in_each_exact_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            token = "2026-08-27-12-22-52-00"
            tree = _grub_rescue_tree(Path(directory), token)
            boot = tree / "boot.efi"
            original = boot.read_bytes()
            boot.write_bytes(original + token.encode("ascii"))
            with self.assertRaisesRegex(WRAPPER.CanonicalizationError, "exactly one"):
                WRAPPER.canonicalize(["-as", "mkisofs", str(tree)], 315532800)
            self.assertIn(token.encode("ascii"), (tree / "efi.img").read_bytes())

    def test_smoke_output_requires_order_no_fatal_and_post_heartbeat_liveness(self) -> None:
        output = "\n".join(ISO.SMOKE_REQUIRED_MARKERS) + "\n"
        self.assertEqual(ISO.validate_smoke_output(output, True), [])
        self.assertIn(
            "causal marker order",
            ISO.validate_smoke_output(
                "\n".join(reversed(ISO.SMOKE_REQUIRED_MARKERS)) + "\n", True
            ),
        )
        self.assertTrue(
            any(
                issue.startswith("forbidden=")
                for issue in ISO.validate_smoke_output(output + "kernel PANIC\n", True)
            )
        )
        self.assertIn(
            "no bounded post-heartbeat liveness",
            ISO.validate_smoke_output(output, False),
        )

    def test_shell_contract_builds_privately_and_boots_without_kernel_or_network(self) -> None:
        build = (ROOT / "ocore/kernel/build-x86_64-uefi-iso.sh").read_text(
            encoding="utf-8"
        )
        run = (ROOT / "ocore/kernel/run-x86_64-uefi-iso-qemu.sh").read_text(
            encoding="utf-8"
        )
        smoke = (ROOT / "ocore/kernel/smoke-x86_64-uefi-iso-qemu.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn('CANDIDATE="$WORK_DIR/candidate.iso"', build)
        self.assertIn('publish --source "$CANDIDATE" --output "$OUTPUT"', build)
        self.assertIn('--directory="$GRUB_EFI_DIRECTORY"', build)
        self.assertIn('--xorriso="$XORRISO_WRAPPER"', build)
        self.assertIn('media_fd_path = f"/dev/fd/{media_descriptor}"', run)
        self.assertIn("nofollow=True", run)
        self.assertIn("os.set_inheritable(media_descriptor, True)", run)
        self.assertIn("readonly=on,file={media_fd_path}", run)
        self.assertIn('"-nic", "none"', run)
        self.assertNotRegex(run, r"(?m)^\s+-kernel(?:\s|$)")
        self.assertIn('cmp -s "$FIRST" "$SECOND"', smoke)
        self.assertNotIn("chmod 0444", smoke)
        self.assertIn('mktemp -d "$SMOKE_ROOT/.smoke-records.XXXXXX"', smoke)
        self.assertIn('"$FIRST_INSPECT_RECORD"', smoke)
        self.assertIn("metadata != expected_metadata", smoke)
        self.assertIn("pass_fds=(media_descriptor, firmware_descriptor)", smoke)
        self.assertIn("validate_smoke_output(output, sustained_liveness)", smoke)
        self.assertNotRegex(smoke, r"(?m)^\s+\"-kernel\",?$")

    def test_interactive_runner_inherits_pinned_fds_for_comma_paths(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / "fixture.iso"
            fixture.write_bytes(_fixture())
            output = root / "built,comma.iso"
            firmware = root / "firmware,comma.fd"
            firmware.write_bytes(b"synthetic firmware")
            build = root / "fake-build"
            build.write_text(
                "#!/bin/sh\nset -eu\n/bin/cp \"$OSTADIX_TEST_FIXTURE\" \"$1\"\n",
                encoding="utf-8",
            )
            build.chmod(0o755)
            qemu = root / "fake-qemu"
            qemu.write_text(
                """#!/usr/bin/env python3
from pathlib import Path
import sys

arguments = sys.argv[1:]
drives = [arguments[index + 1] for index, value in enumerate(arguments) if value == "-drive"]
if len(drives) != 2 or any("comma" in value for value in arguments):
    raise SystemExit(10)
paths = [value.rsplit("file=", 1)[1] for value in drives]
if any(not value.startswith("/dev/fd/") for value in paths):
    raise SystemExit(11)
payloads = [Path(value).read_bytes() for value in paths]
if not any(b"CD001" in payload for payload in payloads):
    raise SystemExit(12)
if not any(payload == b"synthetic firmware" for payload in payloads):
    raise SystemExit(13)
print("fake QEMU inherited pinned descriptors: PASS")
""",
                encoding="utf-8",
            )
            qemu.chmod(0o755)
            environment = os.environ.copy()
            environment.update(
                {
                    "OCORE_QEMU_BIN": str(qemu),
                    "OSTADIX_ISO_BUILD_SCRIPT": str(build),
                    "OSTADIX_ISO_IMAGE": str(output),
                    "OSTADIX_OVMF_CODE": str(firmware),
                    "OSTADIX_PYTHON": sys.executable,
                    "OSTADIX_TEST_FIXTURE": str(fixture),
                }
            )
            result = subprocess.run(
                [str(ROOT / "ocore/kernel/run-x86_64-uefi-iso-qemu.sh")],
                capture_output=True,
                text=True,
                env=environment,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("inherited pinned descriptors: PASS", result.stdout)

    def test_smoke_uses_private_records_pinned_fds_and_sustained_liveness(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / "fixture.iso"
            fixture.write_bytes(_fixture())
            metadata = ISO.inspect_path(fixture)
            firmware = root / "firmware,comma.fd"
            firmware.write_bytes(b"synthetic firmware")
            smoke_root = root / "smoke,comma"
            smoke_root.mkdir()
            victim = root / "victim.txt"
            victim.write_text("preserve me", encoding="utf-8")
            (smoke_root / "first-build.txt").symlink_to(victim)

            build = root / "fake-build"
            build.write_text(
                """#!/usr/bin/env python3
import os
from pathlib import Path
import shutil
import sys

shutil.copyfile(os.environ["OSTADIX_TEST_FIXTURE"], sys.argv[1])
with open(sys.argv[1], "rb") as published:
    os.fchmod(published.fileno(), 0o444)
for key in (
    "iso-sha256",
    "kernel-sha256",
    "efi-boot-image-sha256",
    "efi-bootloader-sha256",
):
    print(f"{key}: {os.environ['OSTADIX_TEST_' + key.upper().replace('-', '_')]}")
""",
                encoding="utf-8",
            )
            build.chmod(0o755)
            qemu = root / "fake-qemu"
            qemu.write_text(
                """#!/usr/bin/env python3
from pathlib import Path
import sys
import time

arguments = sys.argv[1:]
drives = [arguments[index + 1] for index, value in enumerate(arguments) if value == "-drive"]
if len(drives) != 2 or any("comma" in value for value in arguments):
    raise SystemExit(20)
paths = [value.rsplit("file=", 1)[1] for value in drives]
if any(not value.startswith("/dev/fd/") for value in paths):
    raise SystemExit(21)
payloads = [Path(value).read_bytes() for value in paths]
if not any(b"CD001" in payload for payload in payloads):
    raise SystemExit(22)
if not any(payload == b"synthetic firmware" for payload in payloads):
    raise SystemExit(23)
for marker in (
    "O-core kernel: serial online",
    "page protections: W^X online",
    "CPL3 native[0]: online",
    "timer CPL3 return: online",
    "CPL3 heartbeat: online",
):
    print(marker, flush=True)
time.sleep(10)
""",
                encoding="utf-8",
            )
            qemu.chmod(0o755)
            environment = os.environ.copy()
            environment.update(
                {
                    "OCORE_QEMU_BIN": str(qemu),
                    "OSTADIX_ISO_BUILD_SCRIPT": str(build),
                    "OSTADIX_ISO_SMOKE_ROOT": str(smoke_root),
                    "OSTADIX_ISO_TIMEOUT_SECONDS": "2",
                    "OSTADIX_OVMF_CODE": str(firmware),
                    "OSTADIX_TEST_FIXTURE": str(fixture),
                    "OSTADIX_TEST_ISO_SHA256": str(metadata["sha256"]),
                    "OSTADIX_TEST_KERNEL_SHA256": str(metadata["kernel_sha256"]),
                    "OSTADIX_TEST_EFI_BOOT_IMAGE_SHA256": str(
                        metadata["efi_boot_image_sha256"]
                    ),
                    "OSTADIX_TEST_EFI_BOOTLOADER_SHA256": str(
                        metadata["efi_bootloader_sha256"]
                    ),
                }
            )
            result = subprocess.run(
                [str(ROOT / "ocore/kernel/smoke-x86_64-uefi-iso-qemu.sh")],
                capture_output=True,
                text=True,
                env=environment,
                timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("post-heartbeat liveness=0.500s", result.stdout)
            self.assertEqual(victim.read_text(encoding="utf-8"), "preserve me")
            self.assertEqual(list(smoke_root.glob(".smoke-records.*")), [])

    def test_smoke_rejects_a_zero_timeout_before_building(self) -> None:
        script = ROOT / "ocore/kernel/smoke-x86_64-uefi-iso-qemu.sh"
        environment = os.environ.copy()
        environment.update(
            {
                "OCORE_QEMU_BIN": "true",
                "OSTADIX_ISO_TIMEOUT_SECONDS": "0.000",
            }
        )
        result = subprocess.run(
            [str(script)], capture_output=True, text=True, env=environment
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("greater than zero", result.stderr)


if __name__ == "__main__":
    unittest.main()
