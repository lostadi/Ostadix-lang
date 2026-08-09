import contextlib
import hashlib
import importlib.util
import io
import os
from pathlib import Path
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
        )

    def test_token_is_stable_and_domain_bound(self) -> None:
        token = WRITER.confirmation_token(self.device, "ab" * 32, 1024)
        self.assertEqual(token, WRITER.confirmation_token(self.device, "ab" * 32, 1024))
        self.assertRegex(token, r"^OSTADIX-WRITE-[0-9A-F]{16}$")

    def test_token_changes_with_every_authoritative_field(self) -> None:
        baseline = WRITER.confirmation_token(self.device, "ab" * 32, 1024)
        variants = [
            WRITER.DeviceInfo(**{**self.device.__dict__, "path": "/dev/disk8"}),
            WRITER.DeviceInfo(**{**self.device.__dict__, "identity": "replacement"}),
            WRITER.DeviceInfo(**{**self.device.__dict__, "bytes": self.device.bytes + 1}),
        ]
        for device in variants:
            self.assertNotEqual(baseline, WRITER.confirmation_token(device, "ab" * 32, 1024))
        self.assertNotEqual(baseline, WRITER.confirmation_token(self.device, "cd" * 32, 1024))
        self.assertNotEqual(baseline, WRITER.confirmation_token(self.device, "ab" * 32, 2048))

    def test_public_device_omits_raw_mutation_path(self) -> None:
        public = self.device.public()
        self.assertNotIn("raw_path", public)
        self.assertEqual(public["path"], "/dev/disk9")

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
                    "serial": "fixture-serial",
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

    def test_bounded_v1_requires_exact_device_capacity(self) -> None:
        exact = WRITER.DeviceInfo(**{**self.device.__dict__, "bytes": 4096})
        WRITER._require_exact_capacity(exact, 4096)
        for capacity in (4095, 4097):
            candidate = WRITER.DeviceInfo(
                **{**self.device.__dict__, "bytes": capacity}
            )
            with self.assertRaisesRegex(WRITER.WriterError, "equal image bytes exactly"):
                WRITER._require_exact_capacity(candidate, 4096)

    def test_capacity_mismatch_fails_before_device_copy(self) -> None:
        payload = b"bounded-source-image"
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "source.img"
            image.write_bytes(payload)
            mismatched = WRITER.DeviceInfo(
                **{**self.device.__dict__, "bytes": len(payload) + 1}
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

    def _write_after_source_mutation(self, mutate) -> tuple[int, mock.Mock]:
        payload = b"bounded-source-image"
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "source.img"
            image.write_bytes(payload)
            device = WRITER.DeviceInfo(
                **{**self.device.__dict__, "bytes": len(payload)}
            )
            token = WRITER.confirmation_token(
                device, hashlib.sha256(payload).hexdigest(), len(payload)
            )
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

    def test_trailing_snapshot_bytes_fail_before_target_open(self) -> None:
        payload = b"admittedtrailing"
        snapshot = WRITER.SourceSnapshot(
            origin=Path("/unread"),
            stream=io.BytesIO(payload),
            sha256=hashlib.sha256(b"admitted").hexdigest(),
            bytes=len(b"admitted"),
            identity=WRITER.SourceIdentity(1, 1, len(b"admitted"), 1, 1),
        )
        with mock.patch("builtins.open") as target_open:
            with self.assertRaisesRegex(WRITER.WriterError, "trailing bytes"):
                WRITER._copy_and_verify(snapshot, self.device)
        target_open.assert_not_called()

    def test_copy_writes_and_verifies_exact_admitted_bytes(self) -> None:
        payload = b"one exact admitted image"
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "device"
            target.write_bytes(b"x" * len(payload))
            device = WRITER.DeviceInfo(
                **{
                    **self.device.__dict__,
                    "raw_path": str(target),
                    "bytes": len(payload),
                }
            )
            snapshot = WRITER.SourceSnapshot(
                origin=Path("/unread"),
                stream=io.BytesIO(payload),
                sha256=hashlib.sha256(payload).hexdigest(),
                bytes=len(payload),
                identity=WRITER.SourceIdentity(1, 1, len(payload), 1, 1),
            )
            WRITER._copy_and_verify(snapshot, device)
            self.assertEqual(target.read_bytes(), payload)


if __name__ == "__main__":
    unittest.main()
