import importlib.util
import io
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "ostadix_boot_media.py"
SPEC = importlib.util.spec_from_file_location("ostadix_boot_media", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MEDIA = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MEDIA
SPEC.loader.exec_module(MEDIA)


class OstadixBootMediaTests(unittest.TestCase):
    @staticmethod
    def _esp() -> bytes:
        esp = bytearray(1024 * 1024)
        esp[0:3] = b"\xeb\x58\x90"
        esp[3:11] = b"MSDOS5.0"
        esp[510:512] = b"\x55\xaa"
        return bytes(esp)

    @staticmethod
    def _materialize_plan(
        source: bytes, plan: object, *, unwritten_byte: int = 0
    ) -> bytes:
        target = bytearray([unwritten_byte]) * plan.target_bytes
        for extent in plan.extents:
            if extent.source_offset is not None:
                content = source[
                    extent.source_offset : extent.source_offset + extent.bytes
                ]
            else:
                content = extent.data
            assert content is not None
            assert len(content) == extent.bytes
            target[
                extent.target_offset : extent.target_offset + extent.bytes
            ] = content
        return bytes(target)

    def test_pack_is_reproducible_and_round_trips(self) -> None:
        first, first_meta = MEDIA.build_image(self._esp())
        second, second_meta = MEDIA.build_image(self._esp())
        self.assertEqual(first, second)
        self.assertEqual(first_meta, second_meta)
        self.assertEqual(MEDIA.inspect_image(first), first_meta)

    def test_exact_capacity_plan_reconstructs_canonical_image(self) -> None:
        image, metadata = MEDIA.build_image(self._esp())
        first = MEDIA.plan_target_image(image, len(image))
        second = MEDIA.plan_target_image(image, len(image))
        self.assertEqual(first, second)
        self.assertEqual(first.source_sha256, metadata["sha256"])
        self.assertEqual(first.target_bytes, len(image))
        self.assertEqual(first.unwritten_ranges, ())
        self.assertEqual(first.target_image_sha256, first.source_sha256)
        self.assertEqual(self._materialize_plan(image, first, unwritten_byte=0xA5), image)
        self.assertEqual(first.public()["target_plan_sha256"], first.target_plan_sha256)

    def test_larger_capacity_plans_relocate_mirrored_gpt_and_keep_esp(self) -> None:
        image, metadata = MEDIA.build_image(self._esp())
        esp_start = int(metadata["esp_first_lba"]) * MEDIA.SECTOR_SIZE
        esp_end = (int(metadata["esp_last_lba"]) + 1) * MEDIA.SECTOR_SIZE
        digests: set[str] = set()
        for extra_sectors in (1, 17, 33, 4096):
            with self.subTest(extra_sectors=extra_sectors):
                target_bytes = len(image) + extra_sectors * MEDIA.SECTOR_SIZE
                plan = MEDIA.plan_target_image(image, target_bytes)
                materialized = self._materialize_plan(
                    image, plan, unwritten_byte=0xA5
                )
                sectors = target_bytes // MEDIA.SECTOR_SIZE
                primary = MEDIA._validated_header(materialized, 1)
                backup = MEDIA._validated_header(materialized, sectors - 1)
                self.assertEqual(primary["backup"], sectors - 1)
                self.assertEqual(backup["backup"], 1)
                self.assertEqual(primary["entries"], backup["entries"])
                self.assertEqual(primary["last_usable"], sectors - 34)
                self.assertEqual(backup["last_usable"], sectors - 34)
                self.assertEqual(materialized[esp_start:esp_end], image[esp_start:esp_end])
                if not plan.unwritten_ranges:
                    self.assertEqual(
                        plan.target_image_sha256,
                        MEDIA.hashlib.sha256(materialized).hexdigest(),
                    )
                else:
                    self.assertIsNone(plan.target_image_sha256)
                self.assertEqual(
                    materialized[510:512], b"\x55\xaa"
                )
                _, _, _, _, _, mbr_sectors = MEDIA.struct.unpack_from(
                    "<B3sB3sII", materialized, 446
                )
                self.assertEqual(mbr_sectors, min(sectors - 1, 0xFFFF_FFFF))
                digests.add(plan.target_plan_sha256)
        self.assertEqual(len(digests), 4)

    def test_one_sector_larger_retires_only_nonoverlapping_old_backup_prefix(self) -> None:
        image, _ = MEDIA.build_image(self._esp())
        plan = MEDIA.plan_target_image(image, len(image) + MEDIA.SECTOR_SIZE)
        retired = [
            extent
            for extent in plan.extents
            if extent.kind == "retired-source-backup-gpt"
        ]
        self.assertEqual(len(retired), 1)
        self.assertEqual(retired[0].bytes, MEDIA.SECTOR_SIZE)
        self.assertEqual(retired[0].data, bytes(MEDIA.SECTOR_SIZE))
        for left, right in zip(plan.extents, plan.extents[1:]):
            self.assertLessEqual(
                left.target_offset + left.bytes, right.target_offset
            )

    def test_large_capacity_plan_is_constant_space_and_bounded(self) -> None:
        image, _ = MEDIA.build_image(self._esp())
        plan = MEDIA.plan_target_image(image, MEDIA.MAX_TARGET_BYTES)
        self.assertEqual(plan.target_bytes, MEDIA.MAX_TARGET_BYTES)
        self.assertEqual(
            plan.target_backup_header_lba,
            MEDIA.MAX_TARGET_BYTES // MEDIA.SECTOR_SIZE - 1,
        )
        self.assertGreater(sum(size for _, size in plan.unwritten_ranges), 0)
        self.assertLess(
            sum(extent.bytes for extent in plan.extents),
            len(image) + 2 * (MEDIA.PARTITION_TABLE_BYTES + MEDIA.SECTOR_SIZE),
        )
        with self.assertRaisesRegex(MEDIA.MediaError, "bounded.*maximum"):
            MEDIA.plan_target_image(
                image, MEDIA.MAX_TARGET_BYTES + MEDIA.SECTOR_SIZE
            )

    def test_target_capacity_rejects_smaller_and_unaligned_values(self) -> None:
        image, _ = MEDIA.build_image(self._esp())
        for target_bytes, message in (
            (len(image) - MEDIA.SECTOR_SIZE, "smaller"),
            (len(image) + 1, "multiple of 512"),
        ):
            with self.subTest(target_bytes=target_bytes):
                with self.assertRaisesRegex(MEDIA.MediaError, message):
                    MEDIA.plan_target_image(image, target_bytes)

    def test_target_plan_still_requires_canonical_source_tail(self) -> None:
        image, _ = MEDIA.build_image(self._esp())
        noncanonical = image + bytes(MEDIA.SECTOR_SIZE)
        with self.assertRaises(MEDIA.MediaError):
            MEDIA.plan_target_image(noncanonical, len(noncanonical))

    def test_materialized_target_crc_tamper_is_detected(self) -> None:
        image, _ = MEDIA.build_image(self._esp())
        plan = MEDIA.plan_target_image(image, len(image) + 64 * MEDIA.SECTOR_SIZE)
        materialized = bytearray(self._materialize_plan(image, plan))
        backup_offset = plan.target_backup_header_lba * MEDIA.SECTOR_SIZE
        materialized[backup_offset + 24] ^= 1
        with self.assertRaisesRegex(MEDIA.MediaError, "header CRC"):
            MEDIA._validated_header(bytes(materialized), plan.target_backup_header_lba)

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

    def test_bounded_input_rejects_symlink_without_reading_target(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            target = directory / "target.img"
            target.write_bytes(b"secret")
            source = directory / "source.img"
            source.symlink_to(target)
            with self.assertRaisesRegex(MEDIA.MediaError, "without following links"):
                MEDIA._read_bounded(source, 1024)

    @staticmethod
    def _fake_stat(*, size: int, inode: int = 10, modified_ns: int = 20) -> object:
        return mock.Mock(
            st_mode=stat.S_IFREG | 0o600,
            st_dev=1,
            st_ino=inode,
            st_size=size,
            st_mtime_ns=modified_ns,
            st_ctime_ns=modified_ns,
        )

    def test_bounded_input_rejects_growth_after_admitted_size(self) -> None:
        class FakeFile(io.BytesIO):
            def fileno(self) -> int:
                return 42

        before = self._fake_stat(size=4)
        with (
            mock.patch.object(MEDIA.os, "open", return_value=42),
            mock.patch.object(MEDIA.os, "fdopen", return_value=FakeFile(b"datax")),
            mock.patch.object(MEDIA.os, "fstat", return_value=before),
        ):
            with self.assertRaisesRegex(MEDIA.MediaError, "grew beyond"):
                MEDIA._read_bounded(Path("/fixture/source.img"), 1024)

    def test_bounded_input_rejects_metadata_change_during_read(self) -> None:
        class FakeFile(io.BytesIO):
            def fileno(self) -> int:
                return 42

        before = self._fake_stat(size=4, modified_ns=20)
        after = self._fake_stat(size=4, modified_ns=21)
        with (
            mock.patch.object(MEDIA.os, "open", return_value=42),
            mock.patch.object(MEDIA.os, "fdopen", return_value=FakeFile(b"data")),
            mock.patch.object(MEDIA.os, "fstat", side_effect=[before, after]),
        ):
            with self.assertRaisesRegex(MEDIA.MediaError, "changed while"):
                MEDIA._read_bounded(Path("/fixture/source.img"), 1024)

    def test_bounded_input_rejects_path_replacement_after_read(self) -> None:
        class FakeFile(io.BytesIO):
            def fileno(self) -> int:
                return 42

        before = self._fake_stat(size=4, inode=10)
        replacement = self._fake_stat(size=4, inode=11)
        with (
            mock.patch.object(MEDIA.os, "open", return_value=42),
            mock.patch.object(MEDIA.os, "fdopen", return_value=FakeFile(b"data")),
            mock.patch.object(MEDIA.os, "fstat", side_effect=[before, before]),
            mock.patch.object(MEDIA.os, "stat", return_value=replacement),
        ):
            with self.assertRaisesRegex(MEDIA.MediaError, "replaced"):
                MEDIA._read_bounded(Path("/fixture/source.img"), 1024)


class OvmfResolverTests(unittest.TestCase):
    resolver = ROOT / "ocore" / "kernel" / "resolve-x86_64-ovmf-code.sh"

    def _resolve(
        self, qemu: Path, *, explicit: Path | None = None
    ) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment["PATH"] = os.pathsep.join(("/usr/bin", "/bin"))
        if explicit is None:
            environment.pop("OSTADIX_OVMF_CODE", None)
        else:
            environment["OSTADIX_OVMF_CODE"] = str(explicit)
        return subprocess.run(
            [
                "bash",
                "-c",
                'set -u; source "$1"; resolve_ostadix_x86_64_ovmf_code "$2"',
                "ovmf-resolver-test",
                str(self.resolver),
                str(qemu),
            ],
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )

    def test_explicit_firmware_has_precedence_and_provenance(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            firmware = root / "caller-selected.fd"
            firmware.write_bytes(b"fixture")
            result = self._resolve(root / "missing-qemu", explicit=firmware)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(firmware))
        self.assertIn(
            f"source=explicit candidate={firmware} status=selected", result.stderr
        )
        self.assertIn(
            f"result=resolved source=explicit path={firmware} searched=1",
            result.stderr,
        )

    def test_invalid_explicit_firmware_fails_without_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            missing = Path(directory) / "missing.fd"
            result = self._resolve(Path(directory) / "missing-qemu", explicit=missing)

        self.assertEqual(result.returncode, 127)
        self.assertEqual(result.stdout, "")
        self.assertIn(f"candidate={missing} status=missing", result.stderr)
        self.assertIn("explicit OSTADIX_OVMF_CODE is not a file", result.stderr)
        self.assertNotIn("source=known-layout", result.stderr)

    def test_qemu_prefix_candidate_is_dynamically_searched(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory)
            qemu = prefix / "bin" / "qemu-system-x86_64"
            firmware = prefix / "share" / "qemu" / "edk2-x86_64-code.fd"
            qemu.parent.mkdir(parents=True)
            firmware.parent.mkdir(parents=True)
            qemu.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            qemu.chmod(0o755)
            firmware.write_bytes(b"fixture")
            result = self._resolve(qemu)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(
            result.stderr,
            rf"source=qemu-prefix candidate={firmware} "
            r"status=(selected|available-not-selected)",
        )
        self.assertIn("ovmf-discovery result=resolved", result.stderr)

    def test_qemu_prefix_discovery_is_bash_32_nounset_safe(self) -> None:
        """The real UEFI runners source the resolver after `set -euo pipefail`."""
        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory)
            qemu = prefix / "bin" / "qemu-system-x86_64"
            firmware = prefix / "share" / "qemu" / "edk2-x86_64-code.fd"
            qemu.parent.mkdir(parents=True)
            firmware.parent.mkdir(parents=True)
            qemu.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            qemu.chmod(0o755)
            firmware.write_bytes(b"fixture")
            result = self._resolve(qemu)

            self.assertEqual(result.returncode, 0, result.stderr)
            # Assert while the synthetic candidate still exists. On hosts
            # with a higher-priority system/Homebrew OVMF, the resolver may
            # validly select that file while still exercising this prefix.
            self.assertTrue(Path(result.stdout.strip()).is_file(), result.stdout)
            self.assertNotIn("unbound variable", result.stderr)


if __name__ == "__main__":
    unittest.main()
