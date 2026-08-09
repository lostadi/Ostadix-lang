import contextlib
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "ostadix_media_writer.py"
SPEC = importlib.util.spec_from_file_location("ostadix_media_writer", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
WRITER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = WRITER
SPEC.loader.exec_module(WRITER)


class OstadixMediaWriterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.device = WRITER.DeviceInfo(
            path="/dev/disk9",
            raw_path="/dev/rdisk9",
            identity="disk9-serial",
            bytes=128 * 1024 * 1024,
            model="test media",
            transport="USB",
            platform="macos",
            rdev=os.makedev(9, 0),
        )

    @staticmethod
    def _canonical_image() -> bytes:
        media = WRITER._boot_media_module()
        esp = bytearray(1024 * 1024)
        esp[0:3] = b"\xeb\x58\x90"
        esp[3:11] = b"MSDOS5.0"
        esp[510:512] = b"\x55\xaa"
        return media.build_image(bytes(esp))[0]

    def _device_for_capacity(self, capacity: int, **changes: object) -> WRITER.DeviceInfo:
        fields = {**self.device.__dict__, "bytes": capacity, **changes}
        return WRITER.DeviceInfo(**fields)

    def _plan(self, capacity: int | None = None) -> tuple[bytes, object]:
        image = self._canonical_image()
        capacity = capacity if capacity is not None else len(image)
        return image, WRITER._boot_media_module().plan_target_image(image, capacity)

    def test_token_is_stable_and_domain_bound(self) -> None:
        image, plan = self._plan()
        device = self._device_for_capacity(len(image))
        token = WRITER.confirmation_token(device, plan)
        self.assertEqual(token, WRITER.confirmation_token(device, plan))
        self.assertEqual(
            token,
            WRITER.confirmation_token_from_public(device.public(), plan.public()),
        )
        self.assertRegex(token, r"^OSTADIX-WRITE-[0-9A-F]{32}$")

    def test_public_confirmation_rejects_incomplete_or_tampered_evidence(self) -> None:
        image, plan = self._plan()
        device = self._device_for_capacity(len(image))
        incomplete_device = device.public()
        incomplete_device.pop("device_number")
        with self.assertRaisesRegex(WRITER.WriterError, "keys differ"):
            WRITER.confirmation_token_from_public(incomplete_device, plan.public())
        tampered_plan = plan.public()
        tampered_plan["target_last_usable_lba"] -= 1
        with self.assertRaisesRegex(WRITER.WriterError, "geometry"):
            WRITER.confirmation_token_from_public(device.public(), tampered_plan)

    def test_token_changes_with_every_authoritative_field(self) -> None:
        image, plan = self._plan()
        device = self._device_for_capacity(len(image))
        baseline = WRITER.confirmation_token(device, plan)
        variants = [
            self._device_for_capacity(len(image), path="/dev/disk8"),
            self._device_for_capacity(len(image), identity="replacement"),
            self._device_for_capacity(len(image), rdev=os.makedev(9, 1)),
        ]
        for variant in variants:
            self.assertNotEqual(baseline, WRITER.confirmation_token(variant, plan))
        _, larger_plan = self._plan(len(image) + 512)
        self.assertNotEqual(
            baseline,
            WRITER.confirmation_token(
                self._device_for_capacity(len(image) + 512), larger_plan
            ),
        )

    def test_public_device_omits_raw_mutation_path(self) -> None:
        public = self.device.public()
        self.assertNotIn("raw_path", public)
        self.assertEqual(public["path"], "/dev/disk9")
        self.assertEqual(public["device_number"], "9:0")

    def test_unsupported_platform_fails_closed(self) -> None:
        with self.assertRaisesRegex(WRITER.WriterError, "unsupported"):
            WRITER.inspect_device("/dev/fake", system="Plan9")

    def test_prepare_rejects_arbitrary_file_before_device_probe(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "not-media.img"
            image.write_bytes(b"not an OSTADIX disk")
            with mock.patch.object(WRITER, "inspect_device") as inspect:
                with self.assertRaisesRegex(
                    WRITER.WriterError, "ostadix_boot_media.py inspect"
                ):
                    WRITER.prepare(image, "/dev/disk9")
            inspect.assert_not_called()

    @staticmethod
    def _linux_inventory(
        *,
        name: str = "sdb",
        rm: object = True,
        transport: object = "usb",
        serial: object = "fixture-serial",
        wwn: object = None,
        mountpoints: object = None,
        children: list[dict[str, object]] | None = None,
    ) -> dict[str, object]:
        return {
            "blockdevices": [
                {
                    "name": name,
                    "path": f"/dev/{name}",
                    "type": "disk",
                    "size": 1_000_000,
                    "ro": False,
                    "rm": rm,
                    "model": "fixture",
                    "serial": serial,
                    "wwn": wwn,
                    "tran": transport,
                    "mountpoints": mountpoints,
                    "children": children or [],
                }
            ]
        }

    def _linux_probe(
        self, inventory: dict[str, object], path: str = "/dev/sdb"
    ) -> WRITER.DeviceInfo:
        with (
            mock.patch.object(WRITER, "_linux_inventory", return_value=inventory),
            mock.patch.object(WRITER, "_run", return_value=b"/dev/root-fixture\n"),
            mock.patch.object(
                WRITER, "_device_node_rdev", return_value=os.makedev(8, 16)
            ),
        ):
            return WRITER._linux_device(path)

    def test_linux_direct_mount_is_rejected(self) -> None:
        inventory = self._linux_inventory(mountpoints=["/media/ostadix"])
        with self.assertRaisesRegex(WRITER.WriterError, "descendants is mounted"):
            self._linux_probe(inventory)

    def test_linux_nested_mounted_descendant_is_rejected(self) -> None:
        inventory = {
            "blockdevices": [
                {
                    "name": "sdb",
                    "path": "/dev/sdb",
                    "type": "disk",
                    "size": 1_000_000,
                    "ro": False,
                    "rm": True,
                    "model": "fixture",
                    "serial": "fixture-serial",
                    "wwn": None,
                    "tran": "usb",
                    "mountpoints": [None],
                    "children": [
                        {
                            "name": "sdb1",
                            "path": "/dev/sdb1",
                            "type": "part",
                            "pkname": "sdb",
                            "mountpoints": [None],
                            "children": [
                                {
                                    "name": "dm-9",
                                    "path": "/dev/mapper/fixture",
                                    "type": "crypt",
                                    "pkname": "sdb1",
                                    "mountpoints": ["/mnt/fixture"],
                                }
                            ],
                        }
                    ],
                }
            ]
        }
        with self.assertRaisesRegex(WRITER.WriterError, "descendants is mounted"):
            self._linux_probe(inventory)

    def test_linux_internal_nvme_is_rejected(self) -> None:
        inventory = self._linux_inventory(
            name="nvme1n1", rm=False, transport="nvme", mountpoints=[None]
        )
        with self.assertRaisesRegex(WRITER.WriterError, "RM=true.*USB"):
            self._linux_probe(inventory, "/dev/nvme1n1")

    def test_linux_unknown_transport_is_rejected(self) -> None:
        inventory = self._linux_inventory(
            rm=False, transport=None, mountpoints=[None]
        )
        with self.assertRaisesRegex(WRITER.WriterError, "RM=true.*USB"):
            self._linux_probe(inventory)

    def test_linux_external_usb_is_accepted_when_not_removable(self) -> None:
        inventory = self._linux_inventory(
            rm=False, transport="USB", mountpoints=[None]
        )
        device = self._linux_probe(inventory)
        self.assertEqual(device.path, "/dev/sdb")
        self.assertEqual(device.transport, "usb")

    def test_linux_removable_is_accepted_with_unknown_transport(self) -> None:
        inventory = self._linux_inventory(rm="1", transport=None, mountpoints=[None])
        device = self._linux_probe(inventory)
        self.assertEqual(device.path, "/dev/sdb")
        self.assertEqual(device.transport, "unknown")

    def test_linux_device_without_serial_or_wwn_is_rejected(self) -> None:
        inventory = self._linux_inventory(
            serial=None, wwn=None, mountpoints=[None]
        )
        with self.assertRaisesRegex(WRITER.WriterError, "SERIAL or WWN"):
            self._linux_probe(inventory)

    def test_linux_wwn_is_accepted_as_stable_identity(self) -> None:
        inventory = self._linux_inventory(
            serial=None, wwn="0x5000fixture", mountpoints=[None]
        )
        device = self._linux_probe(inventory)
        self.assertEqual(device.identity, "wwn:0x5000fixture")

    def test_macos_identity_fails_closed_without_device_serial_or_media_uuid(self) -> None:
        with self.assertRaisesRegex(WRITER.WriterError, "stable device serial or media UUID"):
            WRITER._macos_stable_identity(
                {"DeviceIdentifier": "disk9", "BusProtocol": "USB"}
            )

    def test_macos_usb_port_topology_is_not_accepted_as_device_identity(self) -> None:
        with self.assertRaisesRegex(WRITER.WriterError, "port topology"):
            WRITER._macos_stable_identity(
                {
                    "BusProtocol": "USB",
                    "DeviceTreePath": "IODeviceTree:/fixture/usb@1",
                }
            )

    def test_macos_serial_and_media_uuid_identities_are_accepted(self) -> None:
        cases = (
            ({"SerialNumber": "disk-serial"}, "serialnumber:disk-serial"),
            (
                {"DeviceSerialNumber": "device-serial"},
                "deviceserialnumber:device-serial",
            ),
            ({"MediaUUID": "media-uuid"}, "mediauuid:media-uuid"),
        )
        for info, expected in cases:
            with self.subTest(info=info):
                self.assertEqual(WRITER._macos_stable_identity(info), expected)

    def test_planner_accepts_larger_aligned_device_capacity(self) -> None:
        image = self._canonical_image()
        capacity = len(image) + 4096 * 512
        device = self._device_for_capacity(capacity)
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.img"
            source.write_bytes(image)
            with (
                mock.patch.object(WRITER, "_inspect_boot_media_snapshot"),
                mock.patch.object(WRITER, "inspect_device", return_value=device),
            ):
                prepared_device, plan, token = WRITER.prepare(source, device.path)
        self.assertEqual(prepared_device, device)
        self.assertEqual(plan.target_bytes, capacity)
        self.assertGreater(len(plan.unwritten_ranges), 0)
        self.assertEqual(token, WRITER.confirmation_token(device, plan))

    def test_capacity_mismatch_fails_before_device_copy(self) -> None:
        payload = self._canonical_image()
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "source.img"
            image.write_bytes(payload)
            mismatched = WRITER.DeviceInfo(
                **{**self.device.__dict__, "bytes": len(payload) - 512}
            )
            with (
                mock.patch.object(WRITER, "_inspect_boot_media_snapshot"),
                mock.patch.object(WRITER, "inspect_device", return_value=mismatched),
                mock.patch.object(WRITER, "_copy_and_verify") as copy,
                contextlib.redirect_stderr(io.StringIO()),
            ):
                status = WRITER.main(
                    [
                        "write",
                        "--image",
                        str(image),
                        "--device",
                        mismatched.path,
                        "--confirm",
                        "OSTADIX-WRITE-NOT-REACHED",
                    ]
                )
        self.assertEqual(status, 2)
        copy.assert_not_called()

    def test_unaligned_capacity_fails_before_device_copy(self) -> None:
        payload = self._canonical_image()
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "source.img"
            image.write_bytes(payload)
            unaligned = self._device_for_capacity(len(payload) + 1)
            with (
                mock.patch.object(WRITER, "_inspect_boot_media_snapshot"),
                mock.patch.object(WRITER, "inspect_device", return_value=unaligned),
                mock.patch.object(WRITER, "_copy_and_verify") as copy,
                contextlib.redirect_stderr(io.StringIO()),
            ):
                status = WRITER.main(
                    [
                        "write",
                        "--image",
                        str(image),
                        "--device",
                        unaligned.path,
                        "--confirm",
                        "OSTADIX-WRITE-NOT-REACHED",
                    ]
                )
        self.assertEqual(status, 2)
        copy.assert_not_called()

    def test_stale_confirmation_for_previous_capacity_never_writes(self) -> None:
        payload = self._canonical_image()
        old_device = self._device_for_capacity(len(payload) + 64 * 512)
        old_plan = WRITER._boot_media_module().plan_target_image(
            payload, old_device.bytes
        )
        stale_token = WRITER.confirmation_token(old_device, old_plan)
        current = self._device_for_capacity(len(payload) + 65 * 512)
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "source.img"
            image.write_bytes(payload)
            with (
                mock.patch.object(WRITER, "_inspect_boot_media_snapshot"),
                mock.patch.object(WRITER, "inspect_device", return_value=current),
                mock.patch.object(WRITER, "_copy_and_verify") as copy,
                contextlib.redirect_stderr(io.StringIO()),
            ):
                status = WRITER.main(
                    [
                        "write",
                        "--image",
                        str(image),
                        "--device",
                        current.path,
                        "--confirm",
                        stale_token,
                    ]
                )
        self.assertEqual(status, 2)
        copy.assert_not_called()

    def test_capacity_change_after_token_check_never_writes(self) -> None:
        payload = self._canonical_image()
        admitted = self._device_for_capacity(len(payload) + 64 * 512)
        changed = self._device_for_capacity(len(payload) + 65 * 512)
        plan = WRITER._boot_media_module().plan_target_image(payload, admitted.bytes)
        token = WRITER.confirmation_token(admitted, plan)
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "source.img"
            image.write_bytes(payload)
            with (
                mock.patch.object(WRITER, "_inspect_boot_media_snapshot"),
                mock.patch.object(
                    WRITER, "inspect_device", side_effect=[admitted, changed]
                ),
                mock.patch.object(WRITER, "_copy_and_verify") as copy,
                contextlib.redirect_stderr(io.StringIO()),
            ):
                status = WRITER.main(
                    [
                        "write",
                        "--image",
                        str(image),
                        "--device",
                        admitted.path,
                        "--confirm",
                        token,
                    ]
                )
        self.assertEqual(status, 2)
        copy.assert_not_called()

    def test_path_identity_change_while_descriptor_is_held_never_writes(self) -> None:
        payload = self._canonical_image()
        admitted = self._device_for_capacity(
            len(payload) + 64 * 512, platform="linux"
        )
        changed = self._device_for_capacity(
            admitted.bytes,
            platform="linux",
            rdev=os.makedev(8, 99),
            identity="serial:replacement",
        )
        plan = WRITER._boot_media_module().plan_target_image(payload, admitted.bytes)
        token = WRITER.confirmation_token(admitted, plan)
        held = WRITER.OpenMutationTarget(
            stream=mock.Mock(), rdev=admitted.rdev, bytes=admitted.bytes
        )

        @contextlib.contextmanager
        def opened(_device: WRITER.DeviceInfo):
            yield held

        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "source.img"
            image.write_bytes(payload)
            with (
                mock.patch.object(WRITER, "_inspect_boot_media_snapshot"),
                mock.patch.object(
                    WRITER,
                    "inspect_device",
                    side_effect=[admitted, admitted, changed],
                ),
                mock.patch.object(WRITER, "_open_mutation_target", side_effect=opened),
                mock.patch.object(WRITER, "_copy_and_verify") as copy,
                contextlib.redirect_stderr(io.StringIO()),
            ):
                status = WRITER.main(
                    [
                        "write",
                        "--image",
                        str(image),
                        "--device",
                        admitted.path,
                        "--confirm",
                        token,
                    ]
                )
        self.assertEqual(status, 2)
        copy.assert_not_called()

    def test_prepare_emits_capacity_plan_digest_and_extents(self) -> None:
        payload = self._canonical_image()
        device = self._device_for_capacity(len(payload) + 128 * 512)
        output = io.StringIO()
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "source.img"
            image.write_bytes(payload)
            with (
                mock.patch.object(WRITER, "_inspect_boot_media_snapshot"),
                mock.patch.object(WRITER, "inspect_device", return_value=device),
                contextlib.redirect_stdout(output),
            ):
                status = WRITER.main(
                    [
                        "prepare",
                        "--image",
                        str(image),
                        "--device",
                        device.path,
                    ]
                )
        self.assertEqual(status, 0)
        record = WRITER.json.loads(output.getvalue())
        self.assertEqual(record["schema"], "ostadix.media-write/v2")
        self.assertEqual(record["target_bytes"], device.bytes)
        self.assertEqual(
            record["target_plan_sha256"],
            record["target_plan"]["target_plan_sha256"],
        )
        self.assertEqual(record["source_image_sha256"], record["image_sha256"])
        self.assertEqual(record["esp_sha256"], record["target_plan"]["esp_sha256"])
        self.assertEqual(record["target_extents"], record["target_plan"]["extents"])
        self.assertEqual(
            record["unwritten_ranges"], record["target_plan"]["unwritten_ranges"]
        )
        self.assertIn("recoverable", record["unwritten_policy"])
        self.assertIsNone(record["target_image_sha256"])
        self.assertGreaterEqual(len(record["target_plan"]["extents"]), 3)
        self.assertEqual(record["written"], False)

    def _write_after_source_mutation(self, mutate) -> tuple[int, mock.Mock]:
        payload = self._canonical_image()
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "source.img"
            image.write_bytes(payload)
            device = self._device_for_capacity(len(payload) + 64 * 512)
            plan = WRITER._boot_media_module().plan_target_image(payload, device.bytes)
            token = WRITER.confirmation_token(device, plan)
            probes = 0

            def probe(_path: str) -> WRITER.DeviceInfo:
                nonlocal probes
                probes += 1
                if probes == 1:
                    mutate(image)
                return device

            with (
                mock.patch.object(WRITER, "_inspect_boot_media_snapshot"),
                mock.patch.object(WRITER, "inspect_device", side_effect=probe),
                mock.patch.object(WRITER, "_copy_and_verify") as copy,
                contextlib.redirect_stderr(io.StringIO()),
            ):
                status = WRITER.main(
                    [
                        "write",
                        "--image",
                        str(image),
                        "--device",
                        device.path,
                        "--confirm",
                        token,
                    ]
                )
            return status, copy

    def test_source_growth_fails_before_device_copy(self) -> None:
        def grow(path: Path) -> None:
            with path.open("ab") as stream:
                stream.write(b"growth")

        status, copy = self._write_after_source_mutation(grow)
        self.assertEqual(status, 2)
        copy.assert_not_called()

    def test_source_replacement_fails_before_device_copy(self) -> None:
        def replace(path: Path) -> None:
            replacement = path.with_suffix(".replacement")
            replacement.write_bytes(path.read_bytes())
            os.replace(replacement, path)

        status, copy = self._write_after_source_mutation(replace)
        self.assertEqual(status, 2)
        copy.assert_not_called()

    def test_open_mutation_target_uses_nofollow_exclusive_held_descriptor(self) -> None:
        device = self._device_for_capacity(4096, platform="linux")
        value = mock.Mock(st_mode=stat.S_IFBLK | 0o600, st_rdev=device.rdev)
        stream = io.BytesIO(bytes(device.bytes))
        stream.fileno = mock.Mock(return_value=73)  # type: ignore[method-assign]
        with (
            mock.patch.object(WRITER.os, "open", return_value=73) as opened,
            mock.patch.object(WRITER.os, "fstat", return_value=value),
            mock.patch.object(WRITER, "_fd_capacity", return_value=device.bytes),
            mock.patch.object(WRITER.os, "fdopen", return_value=stream),
        ):
            with WRITER._open_mutation_target(device) as held:
                self.assertEqual(held.rdev, device.rdev)
                self.assertEqual(held.bytes, device.bytes)
                self.assertIs(held.stream, stream)
        flags = opened.call_args.args[1]
        self.assertTrue(flags & getattr(os, "O_NOFOLLOW", 0))
        self.assertTrue(flags & os.O_EXCL)
        opened.assert_called_once_with(device.raw_path, flags)

    def test_open_mutation_target_capacity_mismatch_closes_before_yield(self) -> None:
        device = self._device_for_capacity(4096, platform="linux")
        value = mock.Mock(st_mode=stat.S_IFBLK | 0o600, st_rdev=device.rdev)
        with (
            mock.patch.object(WRITER.os, "open", return_value=74),
            mock.patch.object(WRITER.os, "fstat", return_value=value),
            mock.patch.object(WRITER, "_fd_capacity", return_value=4608),
            mock.patch.object(WRITER.os, "fdopen") as fdopen,
            mock.patch.object(WRITER.os, "close") as close,
        ):
            with self.assertRaisesRegex(WRITER.WriterError, "capacity differs"):
                with WRITER._open_mutation_target(device):
                    self.fail("mismatched target must never be yielded")
        fdopen.assert_not_called()
        close.assert_called_once_with(74)

    def test_held_descriptor_prevents_path_reassignment_from_redirecting_write(self) -> None:
        payload = self._canonical_image()
        capacity = len(payload) + 64 * 512
        plan = WRITER._boot_media_module().plan_target_image(payload, capacity)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "device"
            original = Path(directory) / "held-device"
            path.write_bytes(b"\xa5" * capacity)
            held_stream = path.open("r+b", buffering=0)
            os.replace(path, original)
            path.write_bytes(b"\x5a" * capacity)
            device = self._device_for_capacity(capacity, raw_path=str(path))
            snapshot = WRITER.SourceSnapshot(
                origin=Path("/unread"),
                stream=io.BytesIO(payload),
                sha256=hashlib.sha256(payload).hexdigest(),
                bytes=len(payload),
                identity=WRITER.SourceIdentity(1, 1, len(payload), 1, 1),
            )
            with held_stream:
                held = WRITER.OpenMutationTarget(
                    stream=held_stream, rdev=device.rdev, bytes=device.bytes
                )
                WRITER._copy_and_verify(snapshot, device, plan, held)
            redirected = path.read_bytes()
            mutated_held = original.read_bytes()
        self.assertEqual(redirected, b"\x5a" * capacity)
        self.assertNotEqual(mutated_held, b"\xa5" * capacity)
        for extent in plan.extents:
            actual = mutated_held[
                extent.target_offset : extent.target_offset + extent.bytes
            ]
            self.assertEqual(hashlib.sha256(actual).hexdigest(), extent.sha256)

    def test_trailing_snapshot_bytes_fail_before_target_open(self) -> None:
        payload = b"admittedtrailing"
        snapshot = WRITER.SourceSnapshot(
            origin=Path("/unread"),
            stream=io.BytesIO(payload),
            sha256=hashlib.sha256(b"admitted").hexdigest(),
            bytes=len(b"admitted"),
            identity=WRITER.SourceIdentity(1, 1, len(b"admitted"), 1, 1),
        )
        plan = mock.Mock(target_bytes=self.device.bytes)
        target_stream = mock.Mock()
        target = WRITER.OpenMutationTarget(
            stream=target_stream,
            rdev=self.device.rdev,
            bytes=self.device.bytes,
        )
        with self.assertRaisesRegex(WRITER.WriterError, "trailing bytes"):
            WRITER._copy_and_verify(snapshot, self.device, plan, target)
        target_stream.assert_not_called()

    def test_copy_writes_and_verifies_every_admitted_extent(self) -> None:
        payload = self._canonical_image()
        capacity = len(payload) + 128 * 512
        plan = WRITER._boot_media_module().plan_target_image(payload, capacity)
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "device"
            target.write_bytes(b"\xa5" * capacity)
            device = self._device_for_capacity(
                capacity,
                raw_path=str(target),
            )
            snapshot = WRITER.SourceSnapshot(
                origin=Path("/unread"),
                stream=io.BytesIO(payload),
                sha256=hashlib.sha256(payload).hexdigest(),
                bytes=len(payload),
                identity=WRITER.SourceIdentity(1, 1, len(payload), 1, 1),
            )
            with target.open("r+b", buffering=0) as target_stream:
                held = WRITER.OpenMutationTarget(
                    stream=target_stream, rdev=device.rdev, bytes=device.bytes
                )
                WRITER._copy_and_verify(snapshot, device, plan, held)
            result = target.read_bytes()
        for extent in plan.extents:
            actual = result[
                extent.target_offset : extent.target_offset + extent.bytes
            ]
            self.assertEqual(hashlib.sha256(actual).hexdigest(), extent.sha256)
        for offset, size in plan.unwritten_ranges:
            self.assertEqual(result[offset : offset + size], b"\xa5" * size)
        media = WRITER._boot_media_module()
        primary = media._validated_header(result, 1)
        backup = media._validated_header(
            result, capacity // media.SECTOR_SIZE - 1
        )
        self.assertEqual(primary["entries"], backup["entries"])

    @unittest.skipUnless(hasattr(os, "pwrite"), "positional writes are unavailable")
    def test_readback_corruption_of_any_admitted_extent_fails(self) -> None:
        payload = self._canonical_image()
        capacity = len(payload) + 64 * 512
        plan = WRITER._boot_media_module().plan_target_image(payload, capacity)
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "device"
            target.write_bytes(bytes(capacity))
            device = self._device_for_capacity(capacity, raw_path=str(target))
            snapshot = WRITER.SourceSnapshot(
                origin=Path("/unread"),
                stream=io.BytesIO(payload),
                sha256=hashlib.sha256(payload).hexdigest(),
                bytes=len(payload),
                identity=WRITER.SourceIdentity(1, 1, len(payload), 1, 1),
            )
            original_fsync = os.fsync

            def corrupt_after_write(descriptor: int) -> None:
                original_fsync(descriptor)
                final_extent = plan.extents[-1]
                os.pwrite(descriptor, b"\xff", final_extent.target_offset)

            with mock.patch.object(WRITER.os, "fsync", side_effect=corrupt_after_write):
                with self.assertRaisesRegex(
                    WRITER.WriterError, "post-write verification failed"
                ):
                    with target.open("r+b", buffering=0) as target_stream:
                        held = WRITER.OpenMutationTarget(
                            stream=target_stream,
                            rdev=device.rdev,
                            bytes=device.bytes,
                        )
                        WRITER._copy_and_verify(snapshot, device, plan, held)


if __name__ == "__main__":
    unittest.main()
