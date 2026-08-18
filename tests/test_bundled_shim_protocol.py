#!/usr/bin/env python3
"""Protocol-level regression tests for every bundled backend shim."""

import io
import os
import re
import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BACKENDS = ROOT / "backends"
SHIM_CATALOG = ROOT / "crates" / "ostadix-api" / "src" / "shims.rs"

sys.path.insert(0, str(BACKENDS))
import o_shim_common as wire  # noqa: E402


NULL_OK = {"status": "ok", "value": {"t": "null"}}


def encode_frames(*messages):
    frames = []
    for message in messages:
        payload = wire.cbor_encode(message)
        frames.append(len(payload).to_bytes(4, "big") + payload)
    return b"".join(frames)


def decode_frames(payload):
    stream = io.BytesIO(payload)
    messages = []
    while True:
        message = wire.read_wire_message(stream)
        if message is None:
            return messages
        messages.append(message)


def bundled_shim_names():
    source = SHIM_CATALOG.read_text(encoding="utf-8")
    return re.findall(
        r'\(\s*"([^"/]+_shim\.py)"\s*,\s*include_bytes!',
        source,
    )


class BundledShimProtocolTests(unittest.TestCase):
    maxDiff = None

    def run_python(self, *args, requests):
        environment = os.environ.copy()
        environment["PYTHONDONTWRITEBYTECODE"] = "1"
        return subprocess.run(
            [sys.executable, *map(str, args)],
            cwd=BACKENDS,
            env=environment,
            input=encode_frames(*requests),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=5,
            check=False,
        )

    def assert_clean_protocol_exit(self, completed, expected):
        stderr = completed.stderr.decode("utf-8", errors="replace")
        self.assertEqual(0, completed.returncode, stderr)
        self.assertEqual(expected, decode_frames(completed.stdout), stderr)

    def test_every_bundled_shim_acknowledges_shutdown_and_exits(self):
        shim_names = bundled_shim_names()
        discovered = sorted(path.name for path in BACKENDS.glob("*_shim.py"))
        self.assertEqual(discovered, sorted(shim_names))

        for shim_name in shim_names:
            with self.subTest(shim=shim_name):
                completed = self.run_python(
                    BACKENDS / shim_name,
                    requests=[
                        {"cmd": "ping"},
                        {"cmd": "cleanup"},
                        {"cmd": "shutdown"},
                    ],
                )
                self.assert_clean_protocol_exit(completed, [NULL_OK, NULL_OK, NULL_OK])

    def test_common_loop_callbacks_unknown_command_and_shutdown_framing(self):
        harness = """
from o_shim_common import command_loop, send_ok

def handle_exec(command):
    send_ok({"t": "str", "v": command.get("code", "")})

def handle_cleanup():
    send_ok({"t": "str", "v": "cleaned"})

def handle_ping():
    send_ok({"t": "str", "v": "pong"})

command_loop(handle_exec, handle_cleanup=handle_cleanup, handle_ping=handle_ping)
"""
        completed = self.run_python(
            "-c",
            harness,
            requests=[
                {"cmd": "exec", "code": "executed"},
                {"cmd": "cleanup"},
                {"cmd": "ping"},
                {"cmd": "not-a-command"},
                {"cmd": "shutdown"},
            ],
        )
        self.assert_clean_protocol_exit(
            completed,
            [
                {"status": "ok", "value": {"t": "str", "v": "executed"}},
                {"status": "ok", "value": {"t": "str", "v": "cleaned"}},
                {"status": "ok", "value": {"t": "str", "v": "pong"}},
                {"status": "err", "message": "unknown command: 'not-a-command'"},
                NULL_OK,
            ],
        )


if __name__ == "__main__":
    unittest.main()
