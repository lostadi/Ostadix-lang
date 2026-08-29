import contextlib
from dataclasses import replace
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "ostadix_ventoy_installer.py"
SPEC = importlib.util.spec_from_file_location("ostadix_ventoy_installer", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
VENTOY = importlib.util.module_from_spec(SPEC)
import sys

sys.modules[SPEC.name] = VENTOY
SPEC.loader.exec_module(VENTOY)


class FakeCapacityError(RuntimeError):
    pass


class FakeCapacityModule:
    CapacityIsoError = FakeCapacityError

    @staticmethod
    def inspect_descriptor(descriptor: int, _label: str) -> dict[str, object]:
        size = os.fstat(descriptor).st_size
        data = os.pread(descriptor, size, 0)
        if len(data) != size or data.startswith(b"invalid"):
            raise FakeCapacityError("invalid fixture")
        digest = hashlib.sha256(data).hexdigest()
        return {
            "schema": "ostadix.capacity-iso/v1",
            "architecture": "x86_64",
            "bytes": size,
            "sha256": digest,
            "capacity_lock_sha256": hashlib.sha256(b"lock" + data).hexdigest(),
            "default_entry": "hosted",
            "entries": [
                {
                    "id": "hosted",
                    "adapter": "linux-selection",
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
                }
            ],
            "artifacts": [
                {
                    "iso_path": "/boot/hosted/initramfs.cpio.gz",
                    "role": "linux-initrd",
                },
                {
                    "iso_path": "/boot/hosted/vmlinuz-lts",
                    "role": "linux-kernel",
                },
            ],
        }


class OstadixVentoyInstallerTests(unittest.TestCase):
    NAME = "OSTADIX-Hosted-Live-x86_64-UEFI_VTGRUB2.iso"
    SOURCE = b"OSTADIX fixture capacity ISO bytes\n" * 128

    @staticmethod
    def _medium(volume: Path, *, free_bytes: int = 1024 * 1024 * 1024) -> object:
        value = volume.stat()
        return VENTOY.VentoyMedium(
            whole_device="/dev/disk9",
            whole_identifier="disk9",
            whole_device_number="1:90",
            whole_bytes=128 * 1024 * 1024 * 1024,
            model="Fixture USB",
            bus_protocol="USB",
            volume_device="/dev/disk9s1",
            volume_identifier="disk9s1",
            parent_whole_disk="disk9",
            mountpoint=str(volume),
            volume_name="Ventoy",
            filesystem="exfat",
            volume_uuid="11111111-1111-4111-8111-111111111111",
            partition_uuid="22222222-2222-4222-8222-222222222222",
            volume_bytes=127 * 1024 * 1024 * 1024,
            mount_device=value.st_dev,
            mount_inode=value.st_ino,
            efi_identifier="disk9s2",
            efi_volume_uuid="33333333-3333-4333-8333-333333333333",
            efi_partition_uuid="44444444-4444-4444-8444-444444444444",
            efi_bytes=32 * 1024 * 1024,
            free_bytes=free_bytes,
            allocation_block_bytes=131072,
        )

    @contextlib.contextmanager
    def _fixture(self, *, destination: bytes | None = None, free_bytes: int = 1024 * 1024 * 1024):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            volume = root / "Ventoy"
            volume.mkdir()
            source = root / "source.iso"
            source.write_bytes(self.SOURCE)
            if destination is not None:
                (volume / self.NAME).write_bytes(destination)
            medium = self._medium(volume, free_bytes=free_bytes)
            with (
                mock.patch.object(VENTOY, "_capacity_module", return_value=FakeCapacityModule),
                mock.patch.object(VENTOY, "probe_ventoy", return_value=medium),
            ):
                yield root, source, volume, medium

    @staticmethod
    def _rename(directory: int, source: str, destination: str) -> None:
        try:
            os.stat(destination, dir_fd=directory, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise VENTOY.VentoyError("destination exists")
        os.rename(source, destination, src_dir_fd=directory, dst_dir_fd=directory)

    def test_prepare_token_is_stable_and_bound_to_medium_identity(self) -> None:
        with self._fixture() as (_root, source, volume, medium):
            first = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)
            second = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)
            self.assertEqual(first["confirmation"], second["confirmation"])
            changed = replace(
                medium,
                volume_uuid="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            )
            with mock.patch.object(VENTOY, "probe_ventoy", return_value=changed):
                third = VENTOY.prepare(source, changed.whole_device, volume, self.NAME)
            self.assertNotEqual(first["confirmation"], third["confirmation"])
            self.assertRegex(first["confirmation"], r"^OSTADIX-VENTOY-[0-9A-F]{32}$")

    def test_identical_destination_is_verified_zero_write(self) -> None:
        with self._fixture(destination=self.SOURCE) as (_root, source, volume, medium):
            prepared = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)
            with mock.patch.object(VENTOY, "_copy_absent") as copy:
                result, _ = VENTOY.install(
                    source,
                    medium.whole_device,
                    volume,
                    self.NAME,
                    prepared["confirmation"],
                )
            copy.assert_not_called()
            self.assertEqual(result["status"], "already-current")
            self.assertFalse(result["written"])
            self.assertTrue(result["verified"])
            self.assertEqual((volume / self.NAME).read_bytes(), self.SOURCE)

    def test_divergent_destination_is_never_overwritten(self) -> None:
        divergent = b"older valid-looking capacity ISO"
        with self._fixture(destination=divergent) as (_root, source, volume, medium):
            prepared = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)
            self.assertEqual(prepared["status"], "refuse-divergent")
            with (
                mock.patch.object(VENTOY, "_copy_absent") as copy,
                self.assertRaisesRegex(VENTOY.VentoyError, "divergent"),
            ):
                VENTOY.install(
                    source,
                    medium.whole_device,
                    volume,
                    self.NAME,
                    prepared["confirmation"],
                )
            copy.assert_not_called()
            self.assertEqual((volume / self.NAME).read_bytes(), divergent)

    def test_absent_destination_copies_verifies_and_publishes(self) -> None:
        with self._fixture() as (_root, source, volume, medium):
            prepared = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)
            with (
                mock.patch.object(VENTOY, "_rename_exclusive", side_effect=self._rename),
                mock.patch.object(VENTOY, "_full_sync"),
                mock.patch.object(VENTOY, "_sync_directory"),
            ):
                result, _ = VENTOY.install(
                    source,
                    medium.whole_device,
                    volume,
                    self.NAME,
                    prepared["confirmation"],
                )
            self.assertEqual(result["status"], "installed")
            self.assertTrue(result["written"])
            self.assertTrue(result["verified"])
            self.assertEqual((volume / self.NAME).read_bytes(), self.SOURCE)
            self.assertEqual(list(volume.glob(".ostadix-ventoy-*.part")), [])

    def test_stale_confirmation_after_medium_change_never_copies(self) -> None:
        with self._fixture() as (_root, source, volume, medium):
            prepared = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)
            changed = replace(medium, whole_device_number="1:91")
            with (
                mock.patch.object(VENTOY, "probe_ventoy", return_value=changed),
                mock.patch.object(VENTOY, "_copy_absent") as copy,
                self.assertRaisesRegex(VENTOY.VentoyError, "confirmation mismatch"),
            ):
                VENTOY.install(
                    source,
                    changed.whole_device,
                    volume,
                    self.NAME,
                    prepared["confirmation"],
                )
            copy.assert_not_called()
            self.assertFalse((volume / self.NAME).exists())

    def test_insufficient_space_fails_before_copy(self) -> None:
        with self._fixture(free_bytes=1) as (_root, source, volume, medium):
            prepared = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)
            with (
                mock.patch.object(VENTOY, "_copy_absent") as copy,
                self.assertRaisesRegex(VENTOY.VentoyError, "enough free space"),
            ):
                VENTOY.install(
                    source,
                    medium.whole_device,
                    volume,
                    self.NAME,
                    prepared["confirmation"],
                )
            copy.assert_not_called()

    def test_bad_confirmation_fails_before_copy(self) -> None:
        with self._fixture() as (_root, source, volume, medium):
            with (
                mock.patch.object(VENTOY, "_copy_absent") as copy,
                self.assertRaisesRegex(VENTOY.VentoyError, "confirmation mismatch"),
            ):
                VENTOY.install(source, medium.whole_device, volume, self.NAME, "wrong")
            copy.assert_not_called()

    def test_atomic_rename_failure_cleans_owned_temporary(self) -> None:
        with self._fixture() as (_root, source, volume, medium):
            prepared = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)
            with (
                mock.patch.object(VENTOY, "_rename_exclusive", side_effect=VENTOY.VentoyError("rename refused")),
                mock.patch.object(VENTOY, "_full_sync"),
                self.assertRaisesRegex(VENTOY.VentoyError, "rename refused"),
            ):
                VENTOY.install(
                    source,
                    medium.whole_device,
                    volume,
                    self.NAME,
                    prepared["confirmation"],
                )
            self.assertFalse((volume / self.NAME).exists())
            self.assertEqual(list(volume.glob(".ostadix-ventoy-*.part")), [])

    def test_unsupported_exclusive_rename_uses_verified_o_excl_copy(self) -> None:
        with self._fixture() as (_root, source, volume, medium):
            prepared = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)
            with (
                mock.patch.object(
                    VENTOY,
                    "_rename_exclusive",
                    side_effect=VENTOY.ExclusiveRenameUnsupported("fixture ExFAT"),
                ),
                mock.patch.object(VENTOY, "_full_sync"),
                mock.patch.object(VENTOY, "_sync_directory"),
            ):
                result, _ = VENTOY.install(
                    source,
                    medium.whole_device,
                    volume,
                    self.NAME,
                    prepared["confirmation"],
                )
            self.assertEqual(result["status"], "installed")
            self.assertTrue(result["written"])
            self.assertTrue(result["verified"])
            self.assertEqual((volume / self.NAME).read_bytes(), self.SOURCE)
            self.assertEqual(list(volume.glob(".ostadix-ventoy-*.part")), [])

    def test_exclusive_copy_never_overwrites_destination_that_appears(self) -> None:
        intruder = b"appeared during publication"
        with self._fixture() as (_root, source, volume, medium):
            prepared = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)

            def unsupported(_directory: int, _source: str, _destination: str) -> None:
                (volume / self.NAME).write_bytes(intruder)
                raise VENTOY.ExclusiveRenameUnsupported("fixture ExFAT")

            with (
                mock.patch.object(VENTOY, "_rename_exclusive", side_effect=unsupported),
                mock.patch.object(VENTOY, "_full_sync"),
                self.assertRaisesRegex(VENTOY.VentoyError, "appeared"),
            ):
                VENTOY.install(
                    source,
                    medium.whole_device,
                    volume,
                    self.NAME,
                    prepared["confirmation"],
                )
            self.assertEqual((volume / self.NAME).read_bytes(), intruder)
            self.assertEqual(list(volume.glob(".ostadix-ventoy-*.part")), [])

    def test_failed_exclusive_copy_removes_only_its_owned_partial(self) -> None:
        with self._fixture() as (_root, source, volume, medium):
            prepared = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)

            def fail_copy(_source: int, _size: int, output: int, _label: str) -> str:
                os.write(output, b"partial")
                raise VENTOY.VentoyError("fixture exclusive copy failure")

            with (
                mock.patch.object(
                    VENTOY,
                    "_rename_exclusive",
                    side_effect=VENTOY.ExclusiveRenameUnsupported("fixture ExFAT"),
                ),
                mock.patch.object(VENTOY, "_copy_descriptor", side_effect=fail_copy),
                mock.patch.object(VENTOY, "_full_sync"),
                self.assertRaisesRegex(VENTOY.VentoyError, "fixture exclusive copy failure"),
            ):
                VENTOY.install(
                    source,
                    medium.whole_device,
                    volume,
                    self.NAME,
                    prepared["confirmation"],
                )
            self.assertFalse((volume / self.NAME).exists())
            self.assertEqual(list(volume.glob(".ostadix-ventoy-*.part")), [])

    def test_failed_exclusive_copy_preserves_replacement_identity(self) -> None:
        intruder = b"replacement owned by another actor"
        with self._fixture() as (_root, source, volume, medium):
            replacement = volume / "replacement.fixture"
            replacement.write_bytes(intruder)
            prepared = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)

            def replace_then_fail(
                _source: int, _size: int, _output: int, _label: str
            ) -> str:
                (volume / self.NAME).unlink()
                replacement.rename(volume / self.NAME)
                raise VENTOY.VentoyError("fixture exclusive copy failure")

            with (
                mock.patch.object(
                    VENTOY,
                    "_rename_exclusive",
                    side_effect=VENTOY.ExclusiveRenameUnsupported("fixture ExFAT"),
                ),
                mock.patch.object(
                    VENTOY, "_copy_descriptor", side_effect=replace_then_fail
                ),
                mock.patch.object(VENTOY, "_full_sync"),
                self.assertRaisesRegex(
                    VENTOY.VentoyError,
                    "changed identity; refusing unsafe cleanup",
                ),
            ):
                VENTOY.install(
                    source,
                    medium.whole_device,
                    volume,
                    self.NAME,
                    prepared["confirmation"],
                )
            self.assertEqual((volume / self.NAME).read_bytes(), intruder)
            self.assertEqual(list(volume.glob(".ostadix-ventoy-*.part")), [])

    @unittest.skipUnless(sys.platform == "darwin", "renameatx_np is Darwin-specific")
    def test_darwin_rename_exclusive_never_clobbers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.part"
            destination = root / "destination.iso"
            source.write_bytes(b"new")
            destination.write_bytes(b"old")
            descriptor = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
            try:
                with self.assertRaisesRegex(VENTOY.VentoyError, "appeared"):
                    VENTOY._rename_exclusive(descriptor, source.name, destination.name)
                self.assertEqual(source.read_bytes(), b"new")
                self.assertEqual(destination.read_bytes(), b"old")
                destination.unlink()
                VENTOY._rename_exclusive(descriptor, source.name, destination.name)
            finally:
                os.close(descriptor)
            self.assertFalse(source.exists())
            self.assertEqual(destination.read_bytes(), b"new")

    def test_private_copy_corruption_never_publishes(self) -> None:
        with self._fixture() as (_root, source, volume, medium):
            prepared = VENTOY.prepare(source, medium.whole_device, volume, self.NAME)

            def corrupt(_source: object, output: int) -> str:
                os.write(output, b"corrupt")
                return hashlib.sha256(self.SOURCE).hexdigest()

            with (
                mock.patch.object(VENTOY, "_copy_source", side_effect=corrupt),
                mock.patch.object(VENTOY, "_full_sync"),
                mock.patch.object(VENTOY, "_rename_exclusive") as rename,
                self.assertRaises(VENTOY.VentoyError),
            ):
                VENTOY.install(
                    source,
                    medium.whole_device,
                    volume,
                    self.NAME,
                    prepared["confirmation"],
                )
            rename.assert_not_called()
            self.assertFalse((volume / self.NAME).exists())
            self.assertEqual(list(volume.glob(".ostadix-ventoy-*.part")), [])

    def test_verify_rejects_missing_destination(self) -> None:
        with self._fixture() as (_root, source, volume, medium):
            with self.assertRaisesRegex(VENTOY.VentoyError, "does not match"):
                VENTOY.verify(source, medium.whole_device, volume, self.NAME)

    def test_invalid_source_is_rejected_before_device_probe(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "invalid.iso"
            source.write_bytes(b"invalid fixture")
            with (
                mock.patch.object(VENTOY, "_capacity_module", return_value=FakeCapacityModule),
                mock.patch.object(VENTOY, "probe_ventoy") as probe,
                self.assertRaisesRegex(VENTOY.VentoyError, "not a valid"),
            ):
                VENTOY.prepare(source, "/dev/disk9", Path(directory), self.NAME)
            probe.assert_not_called()

    def test_destination_directory_is_rejected(self) -> None:
        with self._fixture() as (_root, source, volume, medium):
            (volume / self.NAME).mkdir()
            with self.assertRaisesRegex(VENTOY.VentoyError, "symlink or special"):
                VENTOY.prepare(source, medium.whole_device, volume, self.NAME)

    def test_destination_name_rejects_traversal(self) -> None:
        with self.assertRaisesRegex(VENTOY.VentoyError, "basename"):
            VENTOY._validate_name("../escape.iso")

    def test_destination_name_requires_ventoy_grub2_suffix(self) -> None:
        with self.assertRaisesRegex(VENTOY.VentoyError, "_VTGRUB2"):
            VENTOY._validate_name("OSTADIX-Hosted-Live-x86_64-UEFI.iso")
        self.assertEqual(VENTOY._validate_name(self.NAME), self.NAME)

    def test_seven_entry_capacity_lab_is_rejected_before_device_probe(self) -> None:
        class LabCapacityModule(FakeCapacityModule):
            @staticmethod
            def inspect_descriptor(descriptor: int, label: str) -> dict[str, object]:
                metadata = FakeCapacityModule.inspect_descriptor(descriptor, label)
                metadata["entries"] = [
                    {"id": entry}
                    for entry in (
                        "hosted",
                        "ostadix",
                        "alpine",
                        "guix",
                        "openbsd",
                        "plan9",
                        "redox",
                    )
                ]
                return metadata

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "capacity-lab.iso"
            source.write_bytes(self.SOURCE)
            with (
                mock.patch.object(VENTOY, "_capacity_module", return_value=LabCapacityModule),
                mock.patch.object(VENTOY, "probe_ventoy") as probe,
                self.assertRaisesRegex(VENTOY.CapacityValidationError, "single-entry physical"),
            ):
                VENTOY.prepare(source, "/dev/disk9", root, self.NAME)
            probe.assert_not_called()

    def _diskutil_fixtures(self, volume: Path) -> tuple[dict[str, dict[str, object]], dict[str, object]]:
        volume_uuid = "11111111-1111-4111-8111-111111111111"
        partition_uuid = "22222222-2222-4222-8222-222222222222"
        efi_volume_uuid = "33333333-3333-4333-8333-333333333333"
        efi_partition_uuid = "44444444-4444-4444-8444-444444444444"
        whole = {
            "WholeDisk": True,
            "DeviceIdentifier": "disk9",
            "Internal": False,
            "OSInternalMedia": False,
            "VirtualOrPhysical": "Physical",
            "Removable": True,
            "RemovableMediaOrExternalDevice": True,
            "Writable": True,
            "WritableMedia": True,
            "BusProtocol": "USB",
            "Content": "GUID_partition_scheme",
            "TotalSize": 128 * 1024 * 1024 * 1024,
            "MediaName": "Fixture USB",
        }
        mounted = {
            "WholeDisk": False,
            "DeviceIdentifier": "disk9s1",
            "ParentWholeDisk": "disk9",
            "MountPoint": str(volume),
            "VolumeName": "Ventoy",
            "FilesystemType": "exfat",
            "Internal": False,
            "Removable": True,
            "RemovableMediaOrExternalDevice": True,
            "Writable": True,
            "WritableMedia": True,
            "WritableVolume": True,
            "BusProtocol": "USB",
            "Bootable": True,
            "VolumeUUID": volume_uuid,
            "DiskUUID": partition_uuid,
            "TotalSize": 127 * 1024 * 1024 * 1024,
            "FreeSpace": 64 * 1024 * 1024 * 1024,
            "VolumeAllocationBlockSize": 131072,
        }
        root = {"ParentWholeDisk": "disk3"}
        inventory = {
            "AllDisksAndPartitions": [
                {
                    "DeviceIdentifier": "disk9",
                    "Partitions": [
                        {
                            "DeviceIdentifier": "disk9s1",
                            "VolumeName": "Ventoy",
                            "VolumeUUID": volume_uuid,
                            "DiskUUID": partition_uuid,
                            "Size": mounted["TotalSize"],
                        },
                        {
                            "DeviceIdentifier": "disk9s2",
                            "VolumeName": "VTOYEFI",
                            "VolumeUUID": efi_volume_uuid,
                            "DiskUUID": efi_partition_uuid,
                            "Size": 32 * 1024 * 1024,
                        },
                    ],
                }
            ]
        }
        return ({"/dev/disk9": whole, "/": root, str(volume): mounted}, inventory)

    def test_diskutil_probe_binds_external_ventoy_layout(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            volume = Path(directory).resolve() / "Ventoy"
            volume.mkdir()
            records, inventory = self._diskutil_fixtures(volume)
            with (
                mock.patch.object(VENTOY, "_diskutil_info", side_effect=lambda target: records[target]),
                mock.patch.object(VENTOY, "_diskutil_list", return_value=inventory),
                mock.patch.object(VENTOY, "_device_number", return_value="1:90"),
            ):
                medium = VENTOY.probe_ventoy("/dev/disk9", volume, system="Darwin")
            self.assertEqual(medium.volume_identifier, "disk9s1")
            self.assertEqual(medium.efi_identifier, "disk9s2")
            self.assertEqual(medium.filesystem, "exfat")

    def test_diskutil_probe_rejects_internal_whole_disk(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            volume = Path(directory).resolve() / "Ventoy"
            volume.mkdir()
            records, inventory = self._diskutil_fixtures(volume)
            records["/dev/disk9"]["Internal"] = True
            with (
                mock.patch.object(VENTOY, "_diskutil_info", side_effect=lambda target: records[target]),
                mock.patch.object(VENTOY, "_diskutil_list", return_value=inventory),
                self.assertRaisesRegex(VENTOY.VentoyError, "external"),
            ):
                VENTOY.probe_ventoy("/dev/disk9", volume, system="Darwin")

    def test_diskutil_probe_rejects_missing_vtoyefi(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            volume = Path(directory).resolve() / "Ventoy"
            volume.mkdir()
            records, inventory = self._diskutil_fixtures(volume)
            inventory["AllDisksAndPartitions"][0]["Partitions"] = inventory["AllDisksAndPartitions"][0]["Partitions"][:1]
            with (
                mock.patch.object(VENTOY, "_diskutil_info", side_effect=lambda target: records[target]),
                mock.patch.object(VENTOY, "_diskutil_list", return_value=inventory),
                mock.patch.object(VENTOY, "_device_number", return_value="1:90"),
                self.assertRaisesRegex(VENTOY.VentoyError, "VTOYEFI"),
            ):
                VENTOY.probe_ventoy("/dev/disk9", volume, system="Darwin")

    def test_install_ejects_only_after_success(self) -> None:
        medium = mock.Mock()
        record = {"status": "already-current", "ejected": False}
        output = io.StringIO()
        with (
            mock.patch.object(VENTOY, "install", return_value=(record, medium)),
            mock.patch.object(VENTOY, "eject") as eject,
            contextlib.redirect_stdout(output),
        ):
            status = VENTOY.main(
                [
                    "install",
                    "--iso",
                    "/source.iso",
                    "--device",
                    "/dev/disk9",
                    "--volume",
                    "/Volumes/Ventoy",
                    "--name",
                    self.NAME,
                    "--confirm",
                    "token",
                    "--eject",
                ]
            )
        self.assertEqual(status, 0)
        eject.assert_called_once_with(medium)
        self.assertTrue(json.loads(output.getvalue())["ejected"])

    def test_install_failure_never_ejects(self) -> None:
        with (
            mock.patch.object(VENTOY, "install", side_effect=VENTOY.VentoyError("failed")),
            mock.patch.object(VENTOY, "eject") as eject,
            contextlib.redirect_stderr(io.StringIO()),
        ):
            status = VENTOY.main(
                [
                    "install",
                    "--iso",
                    "/source.iso",
                    "--device",
                    "/dev/disk9",
                    "--volume",
                    "/Volumes/Ventoy",
                    "--name",
                    self.NAME,
                    "--confirm",
                    "token",
                    "--eject",
                ]
            )
        self.assertEqual(status, 2)
        eject.assert_not_called()


if __name__ == "__main__":
    unittest.main()
