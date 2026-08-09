import importlib.util
import os
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "ostadix_boot_media.py"
SPEC = importlib.util.spec_from_file_location("ostadix_boot_media", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MEDIA = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MEDIA)


class OstadixBootMediaTests(unittest.TestCase):
    @staticmethod
    def _esp() -> bytes:
        esp = bytearray(1024 * 1024)
        esp[0:3] = b"\xeb\x58\x90"
        esp[3:11] = b"MSDOS5.0"
        esp[510:512] = b"\x55\xaa"
        return bytes(esp)

    def test_pack_is_reproducible_and_round_trips(self) -> None:
        first, first_meta = MEDIA.build_image(self._esp())
        second, second_meta = MEDIA.build_image(self._esp())
        self.assertEqual(first, second)
        self.assertEqual(first_meta, second_meta)
        self.assertEqual(MEDIA.inspect_image(first), first_meta)

    def test_esp_must_be_nonempty_bounded_and_sector_aligned(self) -> None:
        for value in (b"", b"x", b"x" * 513):
            with self.subTest(length=len(value)):
                with self.assertRaises(MEDIA.MediaError):
                    MEDIA.build_image(value)

    def test_header_crc_tamper_is_rejected(self) -> None:
        image = bytearray(MEDIA.build_image(self._esp())[0])
        image[MEDIA.SECTOR_SIZE + 24] ^= 1
        with self.assertRaisesRegex(MEDIA.MediaError, "header CRC"):
            MEDIA.inspect_image(bytes(image))

    def test_partition_table_tamper_is_rejected(self) -> None:
        image = bytearray(MEDIA.build_image(self._esp())[0])
        image[2 * MEDIA.SECTOR_SIZE + 40] ^= 1
        with self.assertRaisesRegex(MEDIA.MediaError, "partition-table CRC"):
            MEDIA.inspect_image(bytes(image))

    def test_esp_tamper_breaks_digest_bound_guid(self) -> None:
        image, metadata = MEDIA.build_image(self._esp())
        tampered = bytearray(image)
        offset = int(metadata["esp_first_lba"]) * MEDIA.SECTOR_SIZE + 700
        tampered[offset] ^= 1
        with self.assertRaisesRegex(MEDIA.MediaError, "GUID is not bound"):
            MEDIA.inspect_image(bytes(tampered))

    def test_nonprotective_mbr_is_rejected(self) -> None:
        image = bytearray(MEDIA.build_image(self._esp())[0])
        image[450] = 0
        with self.assertRaisesRegex(MEDIA.MediaError, "protective MBR"):
            MEDIA.inspect_image(bytes(image))

    def test_protective_mbr_range_tamper_is_rejected(self) -> None:
        image = bytearray(MEDIA.build_image(self._esp())[0])
        image[454] ^= 1
        with self.assertRaisesRegex(MEDIA.MediaError, "protective MBR topology"):
            MEDIA.inspect_image(bytes(image))

    def test_extra_legacy_partition_is_rejected(self) -> None:
        image = bytearray(MEDIA.build_image(self._esp())[0])
        image[462] = 0x80
        with self.assertRaisesRegex(MEDIA.MediaError, "protective MBR topology"):
            MEDIA.inspect_image(bytes(image))

    def test_unbound_trailing_space_is_rejected(self) -> None:
        image = MEDIA.build_image(self._esp())[0] + b"\0" * MEDIA.SECTOR_SIZE
        with self.assertRaises(MEDIA.MediaError):
            MEDIA.inspect_image(image)

    def test_nonzero_builder_owned_padding_is_rejected(self) -> None:
        image, metadata = MEDIA.build_image(self._esp())
        esp_offset = int(metadata["esp_first_lba"]) * MEDIA.SECTOR_SIZE
        esp_end = (int(metadata["esp_last_lba"]) + 1) * MEDIA.SECTOR_SIZE
        backup_entries_offset = (
            len(image)
            - MEDIA.SECTOR_SIZE
            - MEDIA.PARTITION_TABLE_BYTES
        )
        mutations = (
            ("protective MBR", 100, "protective MBR reserved region"),
            ("pre-ESP gap", esp_offset - 1, "pre-ESP reserved padding"),
            ("post-ESP gap", esp_end, "post-ESP reserved tail"),
            (
                "primary GPT header tail",
                MEDIA.SECTOR_SIZE + MEDIA.GPT_HEADER_SIZE,
                "GPT header reserved tail",
            ),
            (
                "backup GPT header tail",
                len(image) - MEDIA.SECTOR_SIZE + MEDIA.GPT_HEADER_SIZE,
                "GPT header reserved tail",
            ),
        )
        self.assertLess(esp_end, backup_entries_offset)
        for region, offset, expected_error in mutations:
            with self.subTest(region=region):
                tampered = bytearray(image)
                tampered[offset] = 1
                with self.assertRaisesRegex(MEDIA.MediaError, expected_error):
                    MEDIA.inspect_image(bytes(tampered))

    def test_partition_name_padding_tamper_is_rejected_with_valid_table_crcs(self) -> None:
        image = bytearray(MEDIA.build_image(self._esp())[0])
        primary_entries_offset = 2 * MEDIA.SECTOR_SIZE
        backup_entries_offset = (
            len(image)
            - MEDIA.SECTOR_SIZE
            - MEDIA.PARTITION_TABLE_BYTES
        )
        name_padding_offset = 56 + len("OSTADIX".encode("utf-16-le"))
        image[primary_entries_offset + name_padding_offset] = 1
        image[backup_entries_offset + name_padding_offset] = 1
        entries = image[
            primary_entries_offset : primary_entries_offset + MEDIA.PARTITION_TABLE_BYTES
        ]
        entries_crc = MEDIA.zlib.crc32(entries) & 0xFFFF_FFFF
        for header_offset in (MEDIA.SECTOR_SIZE, len(image) - MEDIA.SECTOR_SIZE):
            MEDIA.struct.pack_into("<I", image, header_offset + 88, entries_crc)
            MEDIA.struct.pack_into("<I", image, header_offset + 16, 0)
            header_crc = MEDIA.zlib.crc32(
                image[header_offset : header_offset + MEDIA.GPT_HEADER_SIZE]
            ) & 0xFFFF_FFFF
            MEDIA.struct.pack_into("<I", image, header_offset + 16, header_crc)
        with self.assertRaisesRegex(MEDIA.MediaError, "partition name or padding"):
            MEDIA.inspect_image(bytes(image))

    def test_atomic_output_creates_and_replaces_regular_files(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            output = directory / "nested" / "ostadix.img"
            MEDIA._write_atomic(output, b"first")
            self.assertEqual(output.read_bytes(), b"first")
            MEDIA._write_atomic(output, b"second")
            self.assertEqual(output.read_bytes(), b"second")

    def test_atomic_output_rejects_symlink_without_touching_target(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            target = directory / "target.img"
            target.write_bytes(b"preserve-me")
            output = directory / "ostadix.img"
            output.symlink_to(target)
            with self.assertRaisesRegex(MEDIA.MediaError, "not a regular file"):
                MEDIA._write_atomic(output, b"replacement")
            self.assertTrue(output.is_symlink())
            self.assertEqual(target.read_bytes(), b"preserve-me")

    @unittest.skipUnless(hasattr(os, "mkfifo"), "FIFO creation is unavailable")
    def test_atomic_output_rejects_fifo(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            output = Path(raw_directory) / "ostadix.img"
            os.mkfifo(output)
            with self.assertRaisesRegex(MEDIA.MediaError, "not a regular file"):
                MEDIA._write_atomic(output, b"replacement")
            self.assertTrue(output.exists())

    def test_atomic_output_rejects_directory(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            output = Path(raw_directory) / "ostadix.img"
            output.mkdir()
            with self.assertRaisesRegex(MEDIA.MediaError, "not a regular file"):
                MEDIA._write_atomic(output, b"replacement")
            self.assertTrue(output.is_dir())


if __name__ == "__main__":
    unittest.main()
