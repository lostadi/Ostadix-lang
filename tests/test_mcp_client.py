"""Real Unix socket/process tests for the persistent short-lived-client bridge."""

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/ostadix_mcp_client.py"
SPEC = importlib.util.spec_from_file_location("ostadix_mcp_client", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
client = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = client
SPEC.loader.exec_module(client)

FAKE_SERVER = r'''
import json, os, pathlib, sys, threading, time
root = pathlib.Path(__file__).parent
with (root / "launches").open("a") as output:
    output.write(str(os.getpid()) + "\n")
lock = threading.Lock()
jobs = set()
cancelled = set()
def send(value):
    with lock:
        print(json.dumps(value), flush=True)
def handle(message):
    request_id = message.get("id")
    method = message.get("method")
    if method == "notifications/cancelled":
        cancelled.add(message["params"]["requestId"])
        (root / "cancelled").write_text(str(message["params"]["requestId"]))
        return
    if request_id is None:
        return
    if method == "initialize":
        result = {"protocolVersion": message["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "fixture", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "echo", "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        name = message["params"]["name"]
        args = message["params"].get("arguments", {})
        if name == "block":
            (root / "blocking").write_text(str(request_id))
            deadline = time.monotonic() + args.get("delay", 5)
            while time.monotonic() < deadline and request_id not in cancelled:
                time.sleep(0.01)
        if name == "delay":
            time.sleep(args.get("seconds", 0.1))
        if name == "o_eval" and args.get("background"):
            time.sleep(args.get("delay", 0.1))
            jobs.add(args["job_id"])
        if name == "mutation":
            (root / "mutation").write_text("executed")
        if name == "large":
            (root / "large-received").write_text(str(len(args["payload"])))
            args = {"bytes": len(args["payload"])}
        if name == "stderr":
            sys.stderr.write("x" * (256 * 1024))
            sys.stderr.flush()
        if name == "background":
            jobs.add(args["job_id"])
        if name == "die":
            os._exit(7)
        value = {"pid": os.getpid(), "name": name, "args": args, "jobs": sorted(jobs)}
        result = {"content": [{"type": "text", "text": json.dumps(value)}], "structuredContent": value, "isError": name == "fail"}
    else:
        result = {}
    send({"jsonrpc": "2.0", "id": request_id, "result": result})
for line in sys.stdin:
    message = json.loads(line)
    if message.get("params", {}).get("name") == "stall_input":
        handle(message)
        time.sleep(message["params"]["arguments"].get("seconds", 1))
        continue
    threading.Thread(target=handle, args=(message,), daemon=True).start()
if (root / "slow-shutdown").exists():
    time.sleep(0.6)
(root / "shutdown").write_text("stdin-closed")
'''


@unittest.skipUnless(os.name == "posix", "the bridge uses local Unix sockets")
class PersistentBridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = tempfile.TemporaryDirectory(prefix="omcp-", dir="/tmp")
        self.root = Path(self.fixture.name).resolve()
        self.backends = self.root / "backends"
        self.backends.mkdir()
        self.binary = self.root / "fake-mcp"
        self.binary.write_text(f"#!{sys.executable}\n" + FAKE_SERVER)
        self.binary.chmod(0o700)
        self.config = client.Configuration(self.binary, self.root, self.backends, self.root / "bridge")

    def tearDown(self) -> None:
        try:
            client.request("bridge/stop", config=self.config, start=False, timeout=2)
            deadline = time.monotonic() + 10
            while self.config.socket_path.exists() and time.monotonic() < deadline:
                time.sleep(0.025)
            self.assertFalse(self.config.socket_path.exists(), "bridge leaked after stop")
        finally:
            self.fixture.cleanup()

    def call(self, name: str, args: dict | None = None, timeout: float = 5) -> dict:
        return client._result(client.request("tools/call", {"name": name, "arguments": args or {}}, timeout, config=self.config))

    def wait_file(self, name: str, timeout: float = 5) -> Path:
        path = self.root / name
        deadline = time.monotonic() + timeout
        while not path.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue(path.exists(), f"fixture omitted {name}")
        return path

    def test_separate_cli_processes_share_background_session(self) -> None:
        environment = self.config.environment()
        first = subprocess.run([sys.executable, str(SCRIPT), "background", '{"job_id":"retained"}'], env=environment, capture_output=True, text=True, timeout=10)
        self.assertEqual(first.returncode, 0, first.stderr + first.stdout)
        second = subprocess.run([sys.executable, str(SCRIPT), "echo", "{}"], env=environment, capture_output=True, text=True, timeout=10)
        self.assertEqual(second.returncode, 0, second.stderr + second.stdout)
        first_value = json.loads(first.stdout)["structuredContent"]
        second_value = json.loads(second.stdout)["structuredContent"]
        self.assertEqual(first_value["pid"], second_value["pid"])
        self.assertEqual(second_value["jobs"], ["retained"])
        self.assertEqual((self.root / "launches").read_text().count("\n"), 1)

    def test_simultaneous_startup_launches_only_one_mcp(self) -> None:
        with ThreadPoolExecutor(max_workers=4) as executor:
            results = list(executor.map(lambda number: self.call("echo", {"number": number}), range(4)))
        self.assertEqual(len({result["structuredContent"]["pid"] for result in results}), 1)
        self.assertEqual((self.root / "launches").read_text().count("\n"), 1)

    def test_inflight_calls_route_out_of_order_without_serializing(self) -> None:
        self.call("echo")
        with ThreadPoolExecutor(max_workers=2) as executor:
            slow = executor.submit(self.call, "block", {"delay": 0.8})
            self.wait_file("blocking")
            fast = executor.submit(self.call, "echo", {"message": "fast"})
            self.assertEqual(fast.result(timeout=0.5)["structuredContent"]["args"], {"message": "fast"})
            self.assertFalse(slow.done())
            self.assertEqual(slow.result(timeout=2)["structuredContent"]["name"], "block")

    def test_disconnection_cancels_only_its_request(self) -> None:
        connection = client._ensure_connection(self.config)
        connection.sendall(client._json_line({"method": "tools/call", "params": {"name": "block", "arguments": {"delay": 5}}, "timeout": 0}))
        active = self.wait_file("blocking").read_text()
        connection.close()
        self.assertEqual(self.wait_file("cancelled").read_text(), active)
        self.assertEqual(self.call("echo")["structuredContent"]["name"], "echo")

    def test_timeout_returns_error_and_cancellation_without_stderr_wait(self) -> None:
        self.call("stderr")
        started = time.monotonic()
        with self.assertRaisesRegex(client.BridgeError, "timed out"):
            self.call("block", {"delay": 5}, timeout=0.15)
        self.assertLess(time.monotonic() - started, 1.5)
        self.wait_file("cancelled")
        self.assertGreater((self.config.directory / f"{self.config.key}.stderr.log").stat().st_size, 200_000)
        self.assertEqual(self.call("echo")["structuredContent"]["name"], "echo")

    def test_nonreading_child_cannot_block_status_timeout_or_shutdown(self) -> None:
        self.call("stall_input", {"seconds": 10})
        started = time.monotonic()
        with self.assertRaisesRegex(client.BridgeError, "execution_uncertain"):
            self.call("large", {"payload": "x" * (2 * 1024 * 1024)}, timeout=0.2)
        status = client._result(client.request("bridge/status", config=self.config))
        self.assertTrue(status["running"])
        self.assertLess(time.monotonic() - started, 1.5)
        stopped = subprocess.run([sys.executable, str(SCRIPT), "--stop"], env=self.config.environment(), capture_output=True, text=True, timeout=10)
        self.assertEqual(stopped.returncode, 0, stopped.stdout + stopped.stderr)
        self.assertLess(time.monotonic() - started, 9)

    def test_timed_out_queued_mutation_is_never_sent_later(self) -> None:
        self.call("stall_input", {"seconds": 1})
        with self.assertRaisesRegex(client.BridgeError, "execution_uncertain"):
            self.call("large", {"payload": "x" * (2 * 1024 * 1024)}, timeout=0.15)
        with self.assertRaisesRegex(client.BridgeError, "cancelled_before_send"):
            self.call("mutation", timeout=0.15)
        self.wait_file("large-received")
        self.call("echo")
        self.assertFalse((self.root / "mutation").exists())

    def test_background_request_survives_disconnect_before_response(self) -> None:
        connection = client._ensure_connection(self.config)
        connection.sendall(client._json_line({"method": "tools/call", "params": {"name": "o_eval", "arguments": {"background": True, "job_id": "recoverable", "delay": 0.2}}, "timeout": 5}))
        connection.close()
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            if "recoverable" in self.call("echo")["structuredContent"]["jobs"]:
                break
            time.sleep(0.025)
        else:
            self.fail("background job disappeared with its short-lived caller")
        self.assertFalse((self.root / "cancelled").exists())

    def test_tool_error_and_raw_tool_catalog_are_preserved(self) -> None:
        result = self.call("fail")
        self.assertTrue(json.loads(client._render_tool(result))["isError"])
        catalog = client._result(client.request("tools/list", config=self.config))
        self.assertEqual(catalog["tools"][0]["name"], "echo")

    def test_explicit_unbounded_cli_and_tool_timeouts_complete(self) -> None:
        for flags, args in (
            (["--timeout", "0"], {"seconds": 0.15}),
            ([], {"seconds": 0.15, "timeout_secs": 0}),
        ):
            with self.subTest(flags=flags, args=args):
                result = subprocess.run(
                    [sys.executable, str(SCRIPT), *flags, "delay", json.dumps(args)],
                    env=self.config.environment(), capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(json.loads(result.stdout)["structuredContent"]["args"], args)

    def test_mcp_exit_does_not_leave_requests_waiting(self) -> None:
        started = time.monotonic()
        with self.assertRaisesRegex(client.BridgeError, "transport closed"):
            self.call("die", timeout=5)
        self.assertLess(time.monotonic() - started, 2)

    def test_stop_closes_mcp_stdin_and_reaps_the_child(self) -> None:
        status = client._result(client.request("bridge/status", config=self.config))
        result = subprocess.run([sys.executable, str(SCRIPT), "--stop"], env=self.config.environment(), capture_output=True, text=True, timeout=12)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertTrue(json.loads(result.stdout)["stopped"])
        self.assertEqual(self.wait_file("shutdown").read_text(), "stdin-closed")
        with self.assertRaises(ProcessLookupError):
            os.kill(status["mcp_pid"], 0)

    def test_private_paths_and_instance_identity(self) -> None:
        self.call("echo")
        self.assertEqual(self.config.directory.stat().st_mode & 0o777, 0o700)
        self.assertEqual(self.config.socket_path.stat().st_mode & 0o777, 0o700)
        changed = client.Configuration(self.binary, self.root, self.root / "other-backends", self.config.directory)
        self.assertNotEqual(self.config.key, changed.key)
        for path in self.config.directory.glob("*.log"):
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_old_shutdown_cannot_unlink_a_replacement_socket(self) -> None:
        old = self.call("echo")["structuredContent"]["pid"]
        (self.root / "slow-shutdown").touch()
        client.request("bridge/stop", config=self.config)
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            connection = client._connect(self.config)
            if connection is None:
                break
            connection.close()
            time.sleep(0.01)
        else:
            self.fail("old listener did not close during shutdown")
        new = self.call("echo")["structuredContent"]["pid"]
        self.assertNotEqual(old, new)
        self.wait_file("shutdown")
        time.sleep(0.15)
        self.assertEqual(self.call("echo")["structuredContent"]["pid"], new)
        self.assertEqual((self.root / "launches").read_text().count("\n"), 2)


class BridgeFormattingTests(unittest.TestCase):
    def test_default_transport_timeout_covers_requested_tool_timeout(self) -> None:
        with patch.dict(os.environ, {"OSTADIX_MCP_CLIENT_TIMEOUT": "600"}):
            self.assertEqual(client._tool_timeout({"timeout_secs": 3600}, None), 3630)
            self.assertEqual(client._tool_timeout({"timeout_secs": 30}, None), 600)
            self.assertEqual(client._tool_timeout({"timeout_secs": 3600}, 10), 10)
            self.assertEqual(client._tool_timeout({"timeout_secs": 0}, None), 0)
            self.assertEqual(client._tool_timeout({"timeout_secs": 30}, 0), 0)
            self.assertEqual(client._tool_timeout({"timeout_secs": 0}, 10), 10)

    def test_legacy_text_success_remains_text(self) -> None:
        self.assertEqual(client._render_tool({"content": [{"type": "text", "text": "SMOKE_OK"}], "isError": False}), "SMOKE_OK")

    def test_timeout_rejects_nan_infinity_negative_and_boolean(self) -> None:
        self.assertEqual(client._positive_timeout(0), 0)
        for value in (-1, float("nan"), float("inf"), True):
            with self.subTest(value=value), self.assertRaises(client.BridgeError):
                client._positive_timeout(value)


if __name__ == "__main__":
    unittest.main()
