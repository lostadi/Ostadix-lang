#!/usr/bin/env python3

import hashlib
import importlib.util
import inspect
from pathlib import Path
import sys
import tempfile
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


SMOKE = _load(
    "ostadix_hosted_live_qemu_vga_smoke_focused",
    ROOT / "ocore/kernel/smoke-x86_64-hosted-live-vga-qemu.py",
)
RELEASE = _load(
    "ostadix_hosted_live_release_for_vga_contract",
    ROOT / "scripts/ostadix_hosted_live_release.py",
)
WASM_PROJECT_SHA256 = b"8" * 64
WASM_STAGED_TREE = b"7" * 40
WASM_ARTIFACT_BYTES = 123
WASM_ARTIFACT_SHA256 = b"6" * 64


class _FakeClock:
    def __init__(self, now: float = 0.0) -> None:
        self.now = now

    def monotonic(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.now += seconds


class _FakeSocket:
    def __init__(
        self,
        clock: _FakeClock,
        *,
        connect_steps: list[tuple[float, object]] | None = None,
        send_steps: list[tuple[float, object]] | None = None,
        recv_steps: list[tuple[float, object]] | None = None,
    ) -> None:
        self.clock = clock
        self.connect_steps = list(connect_steps or [])
        self.send_steps = list(send_steps or [])
        self.recv_steps = list(recv_steps or [])
        self.timeouts: list[float] = []
        self.connected_to: list[str] = []
        self.sent: list[bytes] = []
        self.closed = False

    def _step(self, steps: list[tuple[float, object]]) -> object | None:
        if not steps:
            return None
        advance, outcome = steps.pop(0)
        self.clock.now += advance
        if isinstance(outcome, BaseException):
            raise outcome
        return outcome

    def settimeout(self, seconds: float) -> None:
        self.timeouts.append(seconds)

    def connect(self, path: str) -> None:
        self.connected_to.append(path)
        self._step(self.connect_steps)

    def sendall(self, payload: bytes) -> None:
        self.sent.append(payload)
        self._step(self.send_steps)

    def recv(self, size: int) -> bytes:
        del size
        if not self.recv_steps:
            raise AssertionError("fake socket has no queued receive step")
        outcome = self._step(self.recv_steps)
        if not isinstance(outcome, bytes):
            raise AssertionError(f"fake receive outcome is not bytes: {outcome!r}")
        return outcome

    def close(self) -> None:
        self.closed = True


class HostedLiveDesktopSmokeTests(unittest.TestCase):
    def _complete_markers(self) -> list[bytes]:
        markers = list(SMOKE.REQUIRED_MARKERS)
        replacements = {
            SMOKE.ENTROPY_ORDERED_MARKER: (
                b"OSTADIX HOSTED ENTROPY: PASS device=virtio-rng-pci "
                b"crng_bytes=32 available=256"
            ),
            SMOKE.WASM_MATERIALIZATION_PREFIX.rstrip(): (
                SMOKE.WASM_MATERIALIZATION_PREFIX
                + b"root_sha256="
                + WASM_PROJECT_SHA256
            ),
            SMOKE.WASM_ARTIFACT_PREFIX.rstrip(): (
                SMOKE.WASM_ARTIFACT_PREFIX
                + b"tree="
                + WASM_STAGED_TREE
                + f" bytes={WASM_ARTIFACT_BYTES} sha256=".encode("ascii")
                + WASM_ARTIFACT_SHA256
            ),
        }
        return [replacements.get(marker, marker) for marker in markers]

    def _write_ppm(self, path: Path, colors: list[bytes]) -> None:
        width, height = 320, 200
        pixels = b"".join(colors[index % len(colors)] for index in range(width * height))
        path.write_bytes(f"P6\n{width} {height}\n255\n".encode("ascii") + pixels)

    def _frame(self, pixels: bytes) -> object:
        return SMOKE.Frame(
            path=Path("unused.ppm"),
            width=320,
            height=200,
            pixels=pixels,
            sha256=hashlib.sha256(pixels).hexdigest(),
            nonblack_pixels=320 * 200,
            unique_colors=8,
            chromatic_pixels=320 * 200,
            chromatic_hue_buckets=6,
        )

    def test_marker_chain_requires_desktop_after_live_ready(self) -> None:
        complete_markers = self._complete_markers()
        complete = b"\n".join(complete_markers)
        SMOKE._require_ordered_markers(complete)

        rootfs_marker = b"OSTADIX HOSTED ROOTFS: PASS bytes="
        overlay_marker = b"OSTADIX HOSTED ROOTFS OVERLAY: PASS"
        apk_marker = b"OSTADIX HOSTED APK: PASS"
        self.assertLess(
            SMOKE.REQUIRED_MARKERS.index(rootfs_marker),
            SMOKE.REQUIRED_MARKERS.index(overlay_marker),
        )
        self.assertLess(
            SMOKE.REQUIRED_MARKERS.index(overlay_marker),
            SMOKE.REQUIRED_MARKERS.index(apk_marker),
        )

        out_of_order = b"\n".join(
            (*complete_markers[:-2], SMOKE.DESKTOP_READY_MARKER,
             complete_markers[-2])
        )
        with self.assertRaisesRegex(SMOKE.VisualSmokeError, "omitted marker"):
            SMOKE._require_ordered_markers(out_of_order)

    def test_rootfs_identity_parser_requires_one_full_positive_identity(self) -> None:
        marker = b"OSTADIX HOSTED ROOTFS: PASS bytes=7 sha256=" + b"4" * 64
        self.assertEqual(
            SMOKE._parse_rootfs_identity(b"prefix\r\n" + marker + b"\r\nsuffix\r\n"),
            {"bytes": 7, "sha256": "4" * 64},
        )
        for transcript in (
            b"OSTADIX HOSTED ROOTFS: PASS bytes=0 sha256=" + b"4" * 64 + b"\n",
            marker + b"\n" + marker + b"\n",
        ):
            with self.subTest(transcript=transcript), self.assertRaisesRegex(
                SMOKE.VisualSmokeError, "exactly one full"
            ):
                SMOKE._parse_rootfs_identity(transcript)

        source = inspect.getsource(SMOKE.run_visual_gate)
        self.assertGreater(
            source.index("rootfs_identity = _parse_rootfs_identity"),
            source.index("exit_code = process.wait"),
        )
        self.assertIn('"rootfs": rootfs_identity', source)

    def test_entropy_parser_requires_bound_qemu_device_probe_and_strength(self) -> None:
        marker = (
            b"OSTADIX HOSTED ENTROPY: PASS device=virtio-rng-pci "
            b"crng_bytes=32 available=256"
        )
        self.assertEqual(
            SMOKE._parse_entropy_identity(marker + b"\n"),
            {"device": "virtio-rng-pci", "crng_bytes": 32, "available": 256},
        )
        for transcript in (
            marker + b"\n" + marker + b"\n",
            b"OSTADIX HOSTED ENTROPY: PASS device=virtio-rng-pci "
            b"crng_bytes=32 available=127\n",
        ):
            with self.subTest(transcript=transcript), self.assertRaises(
                SMOKE.VisualSmokeError
            ):
                SMOKE._parse_entropy_identity(transcript)

    def test_wasm_identity_parser_requires_one_full_source_bound_chain(self) -> None:
        materialization = (
            SMOKE.WASM_MATERIALIZATION_PREFIX
            + b"root_sha256="
            + WASM_PROJECT_SHA256
        )
        artifact = (
            SMOKE.WASM_ARTIFACT_PREFIX
            + b"tree="
            + WASM_STAGED_TREE
            + f" bytes={WASM_ARTIFACT_BYTES} sha256=".encode("ascii")
            + WASM_ARTIFACT_SHA256
        )
        transcript = b"prefix\r\n" + materialization + b"\r\n" + artifact + b"\r\n"
        parsed_materialization, parsed_artifact, _, _ = SMOKE._parse_wasm_identity(
            transcript
        )
        self.assertEqual(
            parsed_materialization,
            {"root_sha256": WASM_PROJECT_SHA256.decode("ascii")},
        )
        self.assertEqual(
            parsed_artifact,
            {
                "staged_tree": WASM_STAGED_TREE.decode("ascii"),
                "bytes": WASM_ARTIFACT_BYTES,
                "sha256": WASM_ARTIFACT_SHA256.decode("ascii"),
                "materialized_project_sha256": WASM_PROJECT_SHA256.decode("ascii"),
            },
        )

        for malformed in (
            materialization + b"\n" + materialization + b"\n" + artifact + b"\n",
            materialization + b"\n" + artifact.replace(b"bytes=123", b"bytes=0") + b"\n",
        ):
            with self.subTest(malformed=malformed), self.assertRaises(
                SMOKE.VisualSmokeError
            ):
                SMOKE._parse_wasm_identity(malformed)

        source = inspect.getsource(SMOKE.run_visual_gate)
        self.assertGreater(
            source.index("_, wasm_identity, _, _ = _parse_wasm_identity"),
            source.index("exit_code = process.wait"),
        )
        self.assertIn('"olangc_wasm": wasm_identity', source)

    def test_marker_chain_rejects_displaced_entropy_with_a_generic_decoy(self) -> None:
        entropy_index = SMOKE.REQUIRED_MARKERS.index(SMOKE.ENTROPY_ORDERED_MARKER)
        entropy = (
            b"OSTADIX HOSTED ENTROPY: PASS device=virtio-rng-pci "
            b"crng_bytes=32 available=256"
        )
        for placement in ("before-rootfs", "after-node"):
            markers = list(SMOKE.REQUIRED_MARKERS)
            markers.insert(
                0 if placement == "before-rootfs" else entropy_index + 2,
                entropy,
            )
            with self.subTest(placement=placement), self.assertRaisesRegex(
                SMOKE.VisualSmokeError,
                "full Hosted entropy marker did not occupy its ordered position",
            ):
                SMOKE._require_ordered_markers(b"\n".join(markers))

    def test_late_desktop_failure_after_input_is_rejected(self) -> None:
        transcript = b"\n".join(
            (*SMOKE.REQUIRED_MARKERS,
             SMOKE.INPUT_MARKER,
             b"OSTADIX HOSTED DESKTOP: FAIL: Xorg exited")
        )
        with self.assertRaisesRegex(SMOKE.VisualSmokeError, "failure marker 'FAIL'"):
            SMOKE._require_ordered_markers(transcript)

    def test_input_marker_must_follow_desktop_marker(self) -> None:
        self.assertFalse(
            SMOKE._input_marker_after_desktop(
                SMOKE.INPUT_MARKER + b"\n" + SMOKE.DESKTOP_READY_MARKER
            )
        )
        self.assertTrue(
            SMOKE._input_marker_after_desktop(
                SMOKE.DESKTOP_READY_MARKER + b"\n" + SMOKE.INPUT_MARKER
            )
        )

    def test_frame_gate_rejects_text_vt_and_accepts_desktop_palette(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            text_vt = root / "text-vt.ppm"
            # Eight gray levels defeat a naive unique-color threshold while
            # remaining entirely achromatic.
            self._write_ppm(
                text_vt,
                [bytes((level, level, level)) for level in range(32, 256, 28)],
            )
            text_frame = SMOKE.read_frame(text_vt)
            self.assertGreaterEqual(text_frame.unique_colors, SMOKE.MIN_UNIQUE_COLORS)
            with self.assertRaisesRegex(SMOKE.VisualSmokeError, "chromatic area"):
                SMOKE.validate_visible_frame(text_frame)

            desktop = root / "desktop.ppm"
            self._write_ppm(
                desktop,
                [
                    b"\xff\x30\x30",  # red
                    b"\xff\xd7\x30",  # yellow
                    b"\x50\xd0\x70",  # green
                    b"\x30\xd7\xd7",  # cyan
                    b"\x50\x80\xff",  # blue
                    b"\xe0\x50\xd0",  # magenta
                    b"\xcd\xd6\xf4",  # light foreground
                    b"\x1e\x1e\x2e",  # dark desktop background
                ],
            )
            desktop_frame = SMOKE.read_frame(desktop)
            SMOKE.validate_visible_frame(desktop_frame)
            self.assertGreaterEqual(
                desktop_frame.chromatic_hue_buckets,
                SMOKE.MIN_CHROMATIC_HUE_BUCKETS,
            )

    def test_changed_frame_threshold_is_enforced(self) -> None:
        before_pixels = bytes((30, 30, 46)) * (320 * 200)
        before = self._frame(before_pixels)

        too_small_pixels = bytearray(before_pixels)
        for index in range(SMOKE.MIN_CHANGED_PIXELS - 1):
            too_small_pixels[index * 3 : index * 3 + 3] = b"\xff\xff\xff"
        with self.assertRaisesRegex(SMOKE.VisualSmokeError, "did not visibly react"):
            SMOKE.changed_pixel_count(before, self._frame(bytes(too_small_pixels)))

        enough_pixels = bytearray(before_pixels)
        for index in range(SMOKE.MIN_CHANGED_PIXELS):
            enough_pixels[index * 3 : index * 3 + 3] = b"\xff\xff\xff"
        self.assertEqual(
            SMOKE.changed_pixel_count(before, self._frame(bytes(enough_pixels))),
            SMOKE.MIN_CHANGED_PIXELS,
        )

    def test_existing_input_command_is_sent_to_focused_xterm(self) -> None:
        class Monitor:
            def __init__(self) -> None:
                self.commands: list[str] = []
                self.sleeps: list[tuple[float, str]] = []

            def command(self, value: str) -> bytes:
                self.commands.append(value)
                return b"(qemu)"

            def _sleep(self, seconds: float, action: str) -> None:
                self.sleeps.append((seconds, action))

        monitor = Monitor()
        SMOKE._type_command(monitor, SMOKE.INPUT_COMMAND)
        self.assertEqual(
            monitor.commands,
            [f"sendkey {SMOKE._key_name(character)} 35" for character in SMOKE.INPUT_COMMAND],
        )
        self.assertEqual(len(monitor.sleeps), len(SMOKE.INPUT_COMMAND))
        self.assertEqual(monitor.commands[-1], "sendkey ret 35")
        self.assertIn("sendkey shift-dot 35", monitor.commands)

    def test_hmp_rearms_one_absolute_deadline_for_every_socket_operation(self) -> None:
        clock = _FakeClock(100.0)
        fake_socket = _FakeSocket(
            clock,
            send_steps=[(1.0, None), (1.0, None)],
            recv_steps=[(1.0, b"(qemu)"), (2.0, b"ok\r\n(qemu)")],
        )
        with (
            mock.patch.object(SMOKE.socket, "socket", return_value=fake_socket),
            mock.patch.object(SMOKE.time, "monotonic", side_effect=clock.monotonic),
            mock.patch.object(SMOKE.time, "sleep", side_effect=clock.sleep),
        ):
            monitor = SMOKE.Hmp(Path("/fake/qemu.sock"), 110.0)
            try:
                self.assertEqual(monitor.command("info status"), b"ok\r\n(qemu)")
                monitor.quit()
            finally:
                monitor.close()

        self.assertEqual(fake_socket.connected_to, ["/fake/qemu.sock"])
        self.assertEqual(
            fake_socket.sent,
            [b"info status\n", b"quit\n"],
        )
        self.assertEqual(len(fake_socket.timeouts), 5)
        for actual, expected in zip(
            fake_socket.timeouts,
            (10.0, 10.0, 9.0, 8.0, 6.0),
            strict=True,
        ):
            self.assertAlmostEqual(actual, expected)

    def test_hmp_rejects_prompt_completed_after_absolute_deadline(self) -> None:
        clock = _FakeClock(100.0)
        fake_socket = _FakeSocket(
            clock,
            recv_steps=[(0.4, b"(qe"), (0.7, b"mu)")],
        )
        with (
            mock.patch.object(SMOKE.socket, "socket", return_value=fake_socket),
            mock.patch.object(SMOKE.time, "monotonic", side_effect=clock.monotonic),
            mock.patch.object(SMOKE.time, "sleep", side_effect=clock.sleep),
            self.assertRaisesRegex(
                SMOKE.VisualSmokeError,
                "timed out waiting for QEMU monitor prompt",
            ),
        ):
            SMOKE.Hmp(Path("/fake/qemu.sock"), 101.0)

        self.assertTrue(fake_socket.closed)
        self.assertEqual(len(fake_socket.timeouts), 3)
        self.assertAlmostEqual(fake_socket.timeouts[-1], 0.6)

    def test_hmp_converts_socket_timeout_to_visual_smoke_error(self) -> None:
        clock = _FakeClock()
        fake_socket = _FakeSocket(
            clock,
            recv_steps=[(0.0, SMOKE.socket.timeout("monitor stalled"))],
        )
        with (
            mock.patch.object(SMOKE.socket, "socket", return_value=fake_socket),
            mock.patch.object(SMOKE.time, "monotonic", side_effect=clock.monotonic),
            mock.patch.object(SMOKE.time, "sleep", side_effect=clock.sleep),
            self.assertRaisesRegex(
                SMOKE.VisualSmokeError,
                "timed out waiting for QEMU monitor prompt",
            ),
        ):
            SMOKE.Hmp(Path("/fake/qemu.sock"), 1.0)

        self.assertTrue(fake_socket.closed)

    def test_graphical_typing_stops_when_absolute_deadline_expires(self) -> None:
        clock = _FakeClock()
        fake_socket = _FakeSocket(
            clock,
            recv_steps=[(0.0, b"(qemu)"), (0.0, b"(qemu)")],
        )
        with (
            mock.patch.object(SMOKE.socket, "socket", return_value=fake_socket),
            mock.patch.object(SMOKE.time, "monotonic", side_effect=clock.monotonic),
            mock.patch.object(SMOKE.time, "sleep", side_effect=clock.sleep),
        ):
            monitor = SMOKE.Hmp(Path("/fake/qemu.sock"), 0.04)
            try:
                with self.assertRaisesRegex(
                    SMOKE.VisualSmokeError,
                    "timed out typing the graphical input command",
                ):
                    SMOKE._type_command(monitor, "ab")
            finally:
                monitor.close()

        self.assertEqual(fake_socket.sent, [b"sendkey a 35\n"])

    def test_visual_smoke_timeout_accepts_1800_and_rejects_1801(self) -> None:
        SMOKE._validate_timeout_seconds(1800.0)
        with self.assertRaisesRegex(
            SMOKE.VisualSmokeError,
            "timeout must be from 1 through 1800 seconds",
        ):
            SMOKE._validate_timeout_seconds(1801.0)

    def test_success_wait_uses_only_the_remaining_gate_deadline(self) -> None:
        clock = _FakeClock(100.0)
        with mock.patch.object(
            SMOKE.time,
            "monotonic",
            side_effect=clock.monotonic,
        ):
            self.assertEqual(
                SMOKE._remaining_before_deadline(103.0, "waiting for QEMU"),
                3.0,
            )
            clock.now = 103.0
            with self.assertRaisesRegex(
                SMOKE.VisualSmokeError,
                "timed out waiting for QEMU",
            ):
                SMOKE._remaining_before_deadline(103.0, "waiting for QEMU")

        source = inspect.getsource(SMOKE.run_visual_gate)
        self.assertIn(
            "process.wait(timeout=min(5.0, exit_wait_seconds))",
            source,
        )

    def test_receipt_contract_names_desktop_and_no_network(self) -> None:
        self.assertEqual(
            SMOKE.VISUAL_SMOKE_SCHEMA,
            "ostadix.hosted-live-qemu-visual-smoke/v7",
        )
        self.assertEqual(SMOKE.DESKTOP_SESSION, "openbox-xterm")
        self.assertEqual(
            SMOKE.REQUIRED_MARKERS[-5], b"OSTADIX HOSTED X11 FONT: PASS"
        )
        self.assertEqual(
            SMOKE.REQUIRED_MARKERS[-4], b"OSTADIX HOSTED PTY: PASS"
        )
        self.assertEqual(
            SMOKE.REQUIRED_MARKERS[-3], b"OSTADIX HOSTED EVDEV: PASS"
        )
        self.assertEqual(
            SMOKE.REQUIRED_MARKERS[-2],
            b"OSTADIX HOSTED NOTEBOOK GUI READY: PASS",
        )
        self.assertEqual(
            SMOKE.REQUIRED_MARKERS[-1],
            b"OSTADIX HOSTED DESKTOP READY: PASS",
        )
        self.assertEqual(
            tuple(marker.decode("ascii") for marker in SMOKE.REQUIRED_MARKERS),
            RELEASE.REQUIRED_VISUAL_SMOKE_MARKERS,
        )
        source = (ROOT / "ocore/kernel/smoke-x86_64-hosted-live-vga-qemu.py").read_text()
        self.assertIn('"network": "none"', source)
        self.assertIn('"entropy": entropy_identity', source)
        self.assertIn('"iso": iso_identity', source)
        self.assertIn('"rootfs": rootfs_identity', source)
        self.assertIn('"session": DESKTOP_SESSION', source)
        self.assertIn('"evdev_marker": EVDEV_READY_MARKER.decode("ascii")', source)
        self.assertIn(
            '"notebook_gui_marker": NOTEBOOK_GUI_READY_MARKER.decode("ascii")',
            source,
        )
        self.assertIn('"desktop_marker": DESKTOP_READY_MARKER.decode("ascii")', source)

    def test_descriptor_identity_hashes_the_held_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "iso"
            payload = b"descriptor-bound-iso"
            path.write_bytes(payload)
            descriptor, state = SMOKE._open_regular(path, "fixture ISO")
            try:
                self.assertEqual(
                    SMOKE._descriptor_identity(descriptor, state, "fixture ISO"),
                    {
                        "bytes": len(payload),
                        "sha256": hashlib.sha256(payload).hexdigest(),
                    },
                )
            finally:
                SMOKE.os.close(descriptor)

    def test_descriptor_digest_rejects_same_inode_mutation_during_gate(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "iso"
            payload = b"descriptor-bound-iso"
            path.write_bytes(payload)
            descriptor, state = SMOKE._open_regular(path, "fixture ISO")
            try:
                expected = SMOKE._descriptor_identity(
                    descriptor, state, "fixture ISO"
                )
                writer = SMOKE.os.open(path, SMOKE.os.O_WRONLY)
                try:
                    SMOKE.os.pwrite(writer, b"X", 0)
                    SMOKE.os.fsync(writer)
                finally:
                    SMOKE.os.close(writer)
                SMOKE.os.utime(
                    path,
                    ns=(state.st_atime_ns, state.st_mtime_ns),
                )

                # The inode, size, and restored mtime still satisfy the metadata
                # guard; the held-descriptor digest must expose the byte change.
                SMOKE._same_file(descriptor, state, "fixture ISO")
                with self.assertRaisesRegex(
                    SMOKE.VisualSmokeError,
                    "content changed during the visual smoke",
                ):
                    SMOKE._require_unchanged_descriptor(
                        descriptor,
                        state,
                        expected,
                        "fixture ISO",
                    )
            finally:
                SMOKE.os.close(descriptor)

        source = inspect.getsource(SMOKE.run_visual_gate)
        self.assertLess(
            source.index("iso_identity = _descriptor_identity"),
            source.index("process = subprocess.Popen"),
        )
        self.assertGreater(
            source.index("_require_unchanged_descriptor"),
            source.index("exit_code = process.wait"),
        )

    def test_desktop_launcher_uses_openvt_xorg_openbox_and_xterm(self) -> None:
        source = (ROOT / "scripts/ostadix-hosted-live-desktop.sh").read_text()
        for fragment in (
            "openvt -c 1 -s -w",
            "/usr/bin/startx",
            "-nolisten tcp",
            "openbox --sm-disable",
            "xsetroot -solid '#181825'",
            "xterm -geometry 90x28",
            "OSTADIX_NOTEBOOK_BROWSER=/usr/bin/firefox-esr",
            "o-notebook",
            "xprop -root _NET_CLIENT_LIST",
            "OSTADIX HOSTED NOTEBOOK GUI READY: PASS",
            "OSTADIX HOSTED X11 FONT: PASS",
            "os.openpty()",
            "OSTADIX HOSTED PTY: PASS",
            "/dev/input/event*",
            "OSTADIX HOSTED EVDEV: PASS",
            "OSTADIX HOSTED DESKTOP READY: PASS",
            "CARGO_HOME=/root/.cargo",
        ):
            self.assertIn(fragment, source)
        self.assertLess(
            source.index('kill -0 "$window_manager"'),
            source.index("OSTADIX HOSTED DESKTOP READY: PASS"),
        )
        self.assertGreaterEqual(source.count("38;5;"), 6)


if __name__ == "__main__":
    unittest.main()
