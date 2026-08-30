from __future__ import annotations

import base64
import importlib.util
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "ostadix_boot_info_qemu.py"
SPEC = importlib.util.spec_from_file_location("ostadix_boot_info_qemu", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
HARNESS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = HARNESS
SPEC.loader.exec_module(HARNESS)

CHALLENGE = "12" * 32
SOURCE_COMMIT = "ab" * 20


def valid_output(
    challenge: str = CHALLENGE, source_commit: str = SOURCE_COMMIT
) -> str:
    return "".join(
        (
            "O-core kernel: serial online\n",
            "BootInfoV1: malformed fixture rejected\n",
            "BootInfoV1: source pointer and temporary aperture released\n",
            "BootInfoV1: Multiboot2 normalized\n",
            "BootInfoV1: ACPI status valid\n",
            "BootInfoV1: EFI64 boot services exited\n",
            "page protections: W^X online\n",
            "page allocator: online\n",
            "BootInfoV1: firmware allocator window admitted\n",
            f"OSTADIX boot challenge: {challenge}\n",
            f"OSTADIX source commit: {source_commit}\n",
            "entry state: CPU-local online\n",
            "T\n",
            "CPL3 native[0]: online\n",
            "timer CPL3 return: online\n",
            "CPL3 heartbeat: online\n",
        )
    )


FAKE_QEMU = r'''#!/usr/bin/env python3
import base64
import json
import os
import signal
import socket
import sys
import threading
import time


scenario = os.environ["FAKE_QEMU_SCENARIO"]
output = base64.b64decode(os.environ["FAKE_QEMU_OUTPUT_B64"])
qmp_argument = sys.argv[sys.argv.index("-qmp") + 1]
qmp_path = qmp_argument.removeprefix("unix:").split(",", 1)[0]
paused = False
if scenario == "no-qmp-ignore-term":
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
pid_file = os.environ.get("FAKE_QEMU_PID_FILE")
if pid_file:
    with open(pid_file, "w", encoding="ascii") as stream:
        stream.write(str(os.getpid()))


def create_qmp_listener():
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(qmp_path)
    listener.listen(1)
    return listener


def serve_qmp(listener):
    global paused
    try:
        connection, _ = listener.accept()
        with connection:
            connection.sendall(
                json.dumps(
                    {
                        "QMP": {
                            "version": {"qemu": {"major": 10, "minor": 2, "micro": 1}},
                            "capabilities": [],
                        }
                    }
                ).encode() + b"\n"
            )
            stream = connection.makefile("rb")
            for raw in stream:
                request = json.loads(raw)
                command = request["execute"]
                if command == "stop":
                    paused = True
                    result = {}
                elif command == "query-status":
                    result = {"status": "paused" if paused else "running"}
                elif command == "query-cpus-fast":
                    result = [{"cpu-index": 0, "thread-id": os.getpid()}]
                elif command == "human-monitor-command":
                    result = request.get("arguments", {}).get("command-line", "") + ": fake"
                else:
                    result = {}
                connection.sendall(
                    (json.dumps({"return": result, "id": request["id"]}) + "\n").encode()
                )
    finally:
        listener.close()


if scenario != "no-qmp-ignore-term":
    qmp_listener = create_qmp_listener()
    threading.Thread(target=serve_qmp, args=(qmp_listener,), daemon=True).start()

chunk_size = max(1, int(os.environ.get("FAKE_QEMU_CHUNK_SIZE", "7")))
for offset in range(0, len(output), chunk_size):
    os.write(1, output[offset : offset + chunk_size])
    time.sleep(0.002)

if scenario == "early-exit":
    time.sleep(0.01)
    raise SystemExit(0)

while True:
    time.sleep(0.05)
'''


class BootInfoQemuHarnessTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.qemu = self.root / "fake-qemu"
        self.qemu.write_text(textwrap.dedent(FAKE_QEMU), encoding="utf-8")
        self.qemu.chmod(self.qemu.stat().st_mode | stat.S_IXUSR)
        self.firmware = self.root / "firmware.fd"
        self.media = self.root / "media.img"
        self.kernel = self.root / "kernel.elf"
        self.firmware.write_bytes(b"firmware")
        self.media.write_bytes(b"media")
        self.kernel.write_bytes(b"kernel")
        self.transcript = self.root / "mode0.serial"
        self.stderr = self.root / "mode0.stderr"
        self.diagnostic = self.root / "mode0-diagnostic.json"
        self.pid_file = self.root / "fake-qemu.pid"

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def run_fake(
        self,
        scenario: str,
        output: str,
        *,
        completion_timeout: float = 0.4,
        post_lifecycle: float = 0.08,
        qmp_budget: float = 0.25,
    ) -> dict[str, object]:
        environment = {
            "FAKE_QEMU_SCENARIO": scenario,
            "FAKE_QEMU_OUTPUT_B64": base64.b64encode(output.encode()).decode(),
            "FAKE_QEMU_CHUNK_SIZE": "17",
            "FAKE_QEMU_PID_FILE": str(self.pid_file),
        }
        with mock.patch.dict(os.environ, environment):
            return HARNESS.run_challenged_mode0(
                qemu=str(self.qemu),
                firmware=self.firmware,
                media=self.media,
                kernel=self.kernel,
                challenge=CHALLENGE,
                source_commit=SOURCE_COMMIT,
                completion_timeout_seconds=completion_timeout,
                post_lifecycle_seconds=post_lifecycle,
                transcript_path=self.transcript,
                stderr_path=self.stderr,
                diagnostic_path=self.diagnostic,
                qmp_budget_seconds=qmp_budget,
                cleanup_timeout_seconds=0.08,
            )

    def test_fragmented_completion_and_post_lifecycle_survival_pass(self) -> None:
        diagnostic = self.run_fake("success", valid_output())
        self.assertTrue(diagnostic["passed"])
        self.assertEqual(diagnostic["classification"], "success")
        self.assertTrue(diagnostic["survived_after_completion"])
        self.assertEqual(self.transcript.read_text(), valid_output())
        self.assertEqual(diagnostic["qmp"], [{"id": "not-requested-on-success"}])
        self.assertLess(diagnostic["completion_seen_seconds"], 0.4)

    def test_t_only_deadline_fails_and_captures_qmp_state(self) -> None:
        partial = valid_output().split("CPL3 native[0]: online\n", 1)[0]
        diagnostic = self.run_fake(
            "t-only",
            partial,
            completion_timeout=1.0,
            qmp_budget=1.0,
        )
        self.assertFalse(diagnostic["passed"])
        self.assertEqual(
            diagnostic["classification"], "completion-deadline/qemu-alive"
        )
        qmp_ids = [entry["id"] for entry in diagnostic["qmp"]]
        self.assertIn("stop-confirmed", qmp_ids)
        self.assertIn("registers", qmp_ids)
        self.assertIn("pic", qmp_ids)
        self.assertIn("irq", qmp_ids)
        persisted = json.loads(self.diagnostic.read_text())
        self.assertEqual(persisted["classification"], diagnostic["classification"])
        self.assertIn("T\n", [event["line"] for event in persisted["arrival_events"]])

    def test_exit_before_post_lifecycle_window_fails(self) -> None:
        diagnostic = self.run_fake("early-exit", valid_output())
        self.assertFalse(diagnostic["passed"])
        self.assertEqual(
            diagnostic["classification"], "post-completion-window-failed"
        )
        self.assertFalse(diagnostic["survived_after_completion"])

    def test_qmp_and_graceful_cleanup_failure_cannot_mask_deadline(self) -> None:
        partial = valid_output().split("CPL3 native[0]: online\n", 1)[0]
        diagnostic = self.run_fake(
            "no-qmp-ignore-term", partial, completion_timeout=0.12
        )
        self.assertFalse(diagnostic["passed"])
        self.assertEqual(
            diagnostic["classification"], "completion-deadline/qemu-alive"
        )
        self.assertEqual(diagnostic["qmp"][-1]["id"], "capture-failure")
        self.assertEqual(diagnostic["cleanup_action"], "kill")
        self.assertTrue(any(issue.startswith("liveness=") for issue in diagnostic["issues"]))

    def test_serial_capture_overflow_is_bounded_and_fails(self) -> None:
        with mock.patch.object(HARNESS, "MAX_CAPTURE_BYTES", 128):
            diagnostic = self.run_fake(
                "capture-overflow", "x" * 1024, completion_timeout=0.3
            )
        self.assertFalse(diagnostic["passed"])
        self.assertEqual(diagnostic["classification"], "capture-overflow")
        self.assertTrue(diagnostic["capture_overflow"])
        self.assertEqual(diagnostic["stdout_bytes"], 128)
        self.assertEqual(self.transcript.stat().st_size, 128)

    def test_marker_order_count_and_wrong_challenge_fail_closed(self) -> None:
        baseline = valid_output()
        self.assertEqual(
            HARNESS.validate_mode0_output(baseline, CHALLENGE, SOURCE_COMMIT), []
        )

        reversed_markers = baseline.replace(
            "CPL3 native[0]: online\ntimer CPL3 return: online\n",
            "timer CPL3 return: online\nCPL3 native[0]: online\n",
        )
        self.assertIn(
            "challenged mode-0 causal marker order",
            HARNESS.validate_mode0_output(
                reversed_markers, CHALLENGE, SOURCE_COMMIT
            ),
        )

        duplicate = baseline.replace(
            f"OSTADIX boot challenge: {CHALLENGE}\n",
            f"OSTADIX boot challenge: {CHALLENGE}\n" * 2,
        )
        self.assertTrue(
            any(
                issue.startswith("wrong-marker-count=")
                for issue in HARNESS.validate_mode0_output(
                    duplicate, CHALLENGE, SOURCE_COMMIT
                )
            )
        )

        wrong_challenge = "34" * 32
        self.assertTrue(
            any(
                issue.startswith("missing=")
                for issue in HARNESS.validate_mode0_output(
                    baseline, wrong_challenge, SOURCE_COMMIT
                )
            )
        )

        prefixed = baseline.replace(
            "BootInfoV1: source pointer and temporary aperture released\n",
            "prefix BootInfoV1: source pointer and temporary aperture released\n",
        )
        self.assertTrue(
            any(
                issue.startswith("missing=")
                for issue in HARNESS.validate_mode0_output(
                    prefixed, CHALLENGE, SOURCE_COMMIT
                )
            )
        )
        self.assertEqual(
            HARNESS.validate_mode0_output(
                baseline.replace("\n", "\r\n"), CHALLENGE, SOURCE_COMMIT
            ),
            [],
        )

    def test_prefixed_completion_line_does_not_complete(self) -> None:
        prefixed = valid_output().replace(
            "CPL3 heartbeat: online\n", "prefix CPL3 heartbeat: online\n"
        )
        diagnostic = self.run_fake(
            "prefixed-completion", prefixed, completion_timeout=0.2
        )
        self.assertFalse(diagnostic["passed"])
        self.assertEqual(
            diagnostic["classification"], "completion-deadline/qemu-alive"
        )
        self.assertIsNone(diagnostic["completion_seen_seconds"])

    def test_semantic_failure_is_frozen_before_qmp(self) -> None:
        prefixed = valid_output().replace(
            "BootInfoV1: source pointer and temporary aperture released\n",
            "prefix BootInfoV1: source pointer and temporary aperture released\n",
        )
        diagnostic = self.run_fake("semantic-invalid", prefixed)
        self.assertFalse(diagnostic["passed"])
        self.assertEqual(diagnostic["classification"], "semantic-invalid")
        self.assertEqual(diagnostic["liveness_classification"], "success")
        self.assertTrue(diagnostic["verdict_frozen_before_qmp"])
        self.assertIn("stop-confirmed", [entry["id"] for entry in diagnostic["qmp"]])

    def test_keyboard_interrupt_reaps_qemu_and_removes_qmp_directory(self) -> None:
        selector = HARNESS.selectors.DefaultSelector()
        selector_type = type(selector)
        selector.close()
        qmp_dir = self.root / "forced-qmp-directory"
        qmp_dir.mkdir()
        environment = {
            "FAKE_QEMU_SCENARIO": "no-qmp-ignore-term",
            "FAKE_QEMU_OUTPUT_B64": "",
            "FAKE_QEMU_CHUNK_SIZE": "17",
            "FAKE_QEMU_PID_FILE": str(self.pid_file),
        }

        def interrupt_after_qemu_start(*_args: object, **_kwargs: object) -> None:
            deadline = time.monotonic() + 2.0
            while not self.pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            if not self.pid_file.exists():
                self.fail("fake QEMU did not start before injected interrupt")
            raise KeyboardInterrupt

        with (
            mock.patch.dict(os.environ, environment),
            mock.patch.object(HARNESS.tempfile, "mkdtemp", return_value=str(qmp_dir)),
            mock.patch.object(
                selector_type, "select", side_effect=interrupt_after_qemu_start
            ),
        ):
            with self.assertRaises(KeyboardInterrupt):
                HARNESS.run_challenged_mode0(
                    qemu=str(self.qemu),
                    firmware=self.firmware,
                    media=self.media,
                    kernel=self.kernel,
                    challenge=CHALLENGE,
                    source_commit=SOURCE_COMMIT,
                    completion_timeout_seconds=1.0,
                    post_lifecycle_seconds=0.08,
                    transcript_path=self.transcript,
                    stderr_path=self.stderr,
                    diagnostic_path=self.diagnostic,
                    cleanup_timeout_seconds=0.08,
                )
        self.assertFalse(qmp_dir.exists())
        pid = int(self.pid_file.read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

    @unittest.skipUnless(hasattr(signal, "SIGTERM"), "SIGTERM is unavailable")
    def test_sigterm_reaps_qemu_and_removes_qmp_directory(self) -> None:
        signal_tmp = self.root / "signal-tmp"
        signal_tmp.mkdir()
        environment = os.environ.copy()
        environment.update(
            {
                "TMPDIR": str(signal_tmp),
                "FAKE_QEMU_SCENARIO": "no-qmp-ignore-term",
                "FAKE_QEMU_OUTPUT_B64": "",
                "FAKE_QEMU_CHUNK_SIZE": "17",
                "FAKE_QEMU_PID_FILE": str(self.pid_file),
            }
        )
        command = [
            sys.executable,
            str(MODULE_PATH),
            "--qemu",
            str(self.qemu),
            "--firmware",
            str(self.firmware),
            "--media",
            str(self.media),
            "--kernel",
            str(self.kernel),
            "--challenge",
            CHALLENGE,
            "--source-commit",
            SOURCE_COMMIT,
            "--completion-timeout-seconds",
            "5",
            "--post-lifecycle-seconds",
            "1",
            "--transcript",
            str(self.transcript),
            "--stderr",
            str(self.stderr),
            "--diagnostic",
            str(self.diagnostic),
        ]
        harness_process = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
        )
        try:
            deadline = time.monotonic() + 3.0
            while not self.pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(self.pid_file.exists(), "fake QEMU did not start")
            qmp_dirs = list(signal_tmp.glob("ostadix-mode0-qmp.*"))
            self.assertEqual(len(qmp_dirs), 1)
            harness_process.terminate()
            stdout, stderr = harness_process.communicate(timeout=5.0)
            self.assertEqual(stdout, b"")
            self.assertEqual(harness_process.returncode, 128 + signal.SIGTERM)
            self.assertIn(b"interrupted by signal", stderr)
        finally:
            if harness_process.poll() is None:
                harness_process.kill()
                harness_process.wait()
        self.assertEqual(list(signal_tmp.glob("ostadix-mode0-qmp.*")), [])
        pid = int(self.pid_file.read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)


if __name__ == "__main__":
    unittest.main()
