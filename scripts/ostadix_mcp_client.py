#!/usr/bin/env python3
"""Persistent local MCP bridge for short-lived agent function invocations.

Each binary/root/backends tuple owns one private Unix socket and one stdio MCP
session. Run ``TOOL JSON``, ``--list-tools``, ``--status``, or ``--stop``. The
``call_tool`` function remains compatible with existing aichat wrappers.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import errno
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import queue
import select
import signal
import shutil
import socket
import stat
import subprocess
import sys
import threading
import time
from typing import Any


MAX_FRAME_BYTES = 16 * 1024 * 1024
STARTUP_TIMEOUT = 20.0
PROTOCOL_VERSION = "2025-03-26"


class BridgeError(RuntimeError):
    """Local bridge or MCP transport failure."""


def _json_line(value: dict[str, Any]) -> bytes:
    frame = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"
    if len(frame) > MAX_FRAME_BYTES:
        raise BridgeError("JSON frame exceeds the 16 MiB transport bound")
    return frame


def _read_frame(stream: Any) -> dict[str, Any]:
    line = stream.readline(MAX_FRAME_BYTES + 1)
    if not line:
        raise BridgeError("connection closed before a response arrived")
    if len(line) > MAX_FRAME_BYTES or not line.endswith(b"\n"):
        raise BridgeError("JSON frame exceeds the 16 MiB transport bound")
    try:
        value = json.loads(line)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BridgeError("invalid JSON transport frame") from error
    if not isinstance(value, dict):
        raise BridgeError("JSON transport frame must be an object")
    return value


def _positive_timeout(value: Any) -> float:
    if isinstance(value, bool):
        raise BridgeError("timeout must be a nonnegative finite number (0 means unbounded)")
    try:
        seconds = float(value)
    except (TypeError, ValueError) as error:
        raise BridgeError("timeout must be a nonnegative finite number (0 means unbounded)") from error
    if not math.isfinite(seconds) or seconds < 0:
        raise BridgeError("timeout must be a nonnegative finite number (0 means unbounded)")
    return seconds


@dataclass(frozen=True)
class Configuration:
    binary: Path
    root: Path
    backends: Path
    directory: Path

    @classmethod
    def from_environment(cls) -> Configuration:
        home = Path.home()
        requested = os.environ.get("OSTADIX_MCP", str(home / ".local/bin/ostadix-mcp"))
        located = shutil.which(requested) if os.sep not in requested else requested
        binary = Path(located or requested).expanduser().absolute()
        root = Path(os.environ.get("O_LANG_ROOT", str(home / "Ostadix-lang"))).expanduser().resolve()
        backends = Path(os.environ.get("O_BACKENDS_DIR", str(root / "backends"))).expanduser().resolve()
        # macOS TMPDIR can be too long for sockaddr_un. Keep the default path
        # short; the directory itself is checked for owner, mode, and symlinks.
        directory = Path(os.environ.get("OSTADIX_MCP_CLIENT_DIR", f"/tmp/ostadix-mcp-client-{os.getuid()}"))
        return cls(binary, root, backends, directory.expanduser().absolute())

    @property
    def key(self) -> str:
        encoded = json.dumps([str(self.binary), str(self.root), str(self.backends)], separators=(",", ":"))
        return hashlib.sha256(encoded.encode()).hexdigest()[:24]

    @property
    def socket_path(self) -> Path:
        return self.directory / f"{self.key}.sock"

    def private_directory(self) -> None:
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        metadata = self.directory.lstat()
        if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o700:
            raise BridgeError(f"bridge directory must be an owned, non-symlink directory with mode 0700: {self.directory}")
        if len(os.fsencode(self.socket_path)) >= 104:
            raise BridgeError("bridge socket path is too long; set OSTADIX_MCP_CLIENT_DIR to a shorter private directory")

    def environment(self) -> dict[str, str]:
        environment = os.environ.copy()
        environment.update({
            "OSTADIX_MCP": str(self.binary),
            "O_LANG_ROOT": str(self.root),
            "O_BACKENDS_DIR": str(self.backends),
            "OSTADIX_MCP_CLIENT_DIR": str(self.directory),
        })
        environment["PATH"] = os.pathsep.join([
            str(Path.home() / ".local/bin"), str(self.root / "target/release"),
            environment.get("PATH", "/usr/bin:/bin"),
        ])
        return environment


def _private_file(path: Path) -> Any:
    descriptor = os.open(path, os.O_CREAT | os.O_RDWR | os.O_APPEND | os.O_NOFOLLOW, 0o600)
    metadata = os.fstat(descriptor)
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o600:
        os.close(descriptor)
        raise BridgeError(f"bridge file must be owned and mode 0600: {path}")
    return os.fdopen(descriptor, "a+b", buffering=0)


def _connect(config: Configuration) -> socket.socket | None:
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    connection.settimeout(1.0)
    try:
        connection.connect(str(config.socket_path))
        return connection
    except OSError as error:
        connection.close()
        if error.errno in (errno.ENOENT, errno.ECONNREFUSED):
            return None
        raise BridgeError(f"cannot connect to local MCP bridge: {error}") from error


def _remove_stale_socket(config: Configuration) -> None:
    try:
        metadata = config.socket_path.lstat()
    except FileNotFoundError:
        return
    if not stat.S_ISSOCK(metadata.st_mode) or metadata.st_uid != os.getuid():
        raise BridgeError(f"refusing to replace a non-socket or foreign socket: {config.socket_path}")
    config.socket_path.unlink()


def _ensure_connection(config: Configuration) -> socket.socket:
    config.private_directory()
    connected = _connect(config)
    if connected is not None:
        return connected
    if not config.binary.is_file() or not os.access(config.binary, os.X_OK):
        raise BridgeError(f"ostadix-mcp executable not found: {config.binary}")
    if not config.root.is_dir() or not config.backends.is_dir():
        raise BridgeError("O_LANG_ROOT and O_BACKENDS_DIR must name existing directories")
    deadline = time.monotonic() + STARTUP_TIMEOUT
    with _private_file(config.directory / f"{config.key}.lock") as lock:
        while True:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise BridgeError("timed out waiting for local MCP bridge startup lock")
                time.sleep(0.025)
        connected = _connect(config)
        if connected is not None:
            return connected
        _remove_stale_socket(config)
        with _private_file(config.directory / f"{config.key}.bridge.log") as log:
            daemon = subprocess.Popen(
                [sys.executable, str(Path(__file__).absolute()), "--serve"],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log,
                env=config.environment(), cwd=config.root, close_fds=True,
                start_new_session=True,
            )
        while time.monotonic() < deadline:
            connected = _connect(config)
            if connected is not None:
                # Hold and reap the launcher while this caller stays alive;
                # short-lived callers may exit while the detached bridge lives.
                threading.Thread(target=daemon.wait, daemon=True, name="mcp-bridge-reaper").start()
                return connected
            if daemon.poll() is not None:
                raise BridgeError(f"MCP bridge exited {daemon.returncode}; inspect {config.directory / (config.key + '.bridge.log')}")
            time.sleep(0.025)
        daemon.terminate()
        try:
            daemon.wait(timeout=2)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
        raise BridgeError("timed out starting local MCP bridge")


def request(method: str, params: dict[str, Any] | None = None, timeout: float = 600.0, *, config: Configuration | None = None, start: bool = True) -> dict[str, Any]:
    config = config or Configuration.from_environment()
    timeout = _positive_timeout(timeout)
    config.private_directory()
    connection = _ensure_connection(config) if start else _connect(config)
    if connection is None:
        return {"result": {"running": False, "socket": str(config.socket_path)}}
    with connection:
        connection.settimeout(timeout + 2.0 if timeout else None)
        frame = _json_line({"method": method, "params": params or {}, "timeout": timeout})
        try:
            connection.sendall(frame)
            with connection.makefile("rb") as stream:
                return _read_frame(stream)
        except (BridgeError, OSError) as error:
            raise BridgeError(f"bridge transport failed after transmission began; execution outcome may be uncertain; no automatic retry: {error}") from error


class StdioBridge:
    def __init__(self, config: Configuration):
        self.config = config
        self.stopping = threading.Event()
        self.outgoing: queue.Queue[dict[str, Any]] = queue.Queue()
        self.pending_lock = threading.Lock()
        self.pending: dict[int, queue.Queue[dict[str, Any]]] = {}
        self.dispatch: dict[int, str] = {}
        self.next_id = 0
        self.failure: str | None = None
        self.child: subprocess.Popen[bytes] | None = None
        self.started_at = time.time()

    def send(self, message: dict[str, Any]) -> None:
        if self.stopping.is_set() or self.child is None or self.child.poll() is not None:
            raise BridgeError("MCP child is not running")
        self.outgoing.put_nowait(message)

    def write_requests(self) -> None:
        assert self.child is not None and self.child.stdin is not None
        descriptor = self.child.stdin.fileno()
        try:
            while not self.stopping.is_set():
                try:
                    message = self.outgoing.get(timeout=0.1)
                except queue.Empty:
                    continue
                request_id = message.get("id") if "method" in message else None
                if isinstance(request_id, int):
                    with self.pending_lock:
                        if request_id not in self.dispatch:
                            continue
                        self.dispatch[request_id] = "writing"
                remaining = memoryview(_json_line(message))
                while remaining and not self.stopping.is_set():
                    _, writable, _ = select.select([], [descriptor], [], 0.1)
                    if not writable:
                        continue
                    try:
                        written = os.write(descriptor, remaining)
                    except BlockingIOError:
                        continue
                    remaining = remaining[written:]
                if isinstance(request_id, int):
                    with self.pending_lock:
                        if request_id in self.dispatch:
                            self.dispatch[request_id] = "sent"
        except (BridgeError, OSError, ValueError) as error:
            self.fail_pending(f"MCP input transport failed: {error}")
            self.stopping.set()

    def submit(self, method: str, params: dict[str, Any]) -> tuple[int, queue.Queue[dict[str, Any]]]:
        with self.pending_lock:
            if self.failure is not None:
                raise BridgeError(self.failure)
            self.next_id += 1
            request_id = self.next_id
            response: queue.Queue[dict[str, Any]] = queue.Queue(maxsize=1)
            self.pending[request_id] = response
            self.dispatch[request_id] = "queued"
        try:
            self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        except (OSError, BridgeError):
            with self.pending_lock:
                self.pending.pop(request_id, None)
                self.dispatch.pop(request_id, None)
            raise
        return request_id, response

    def fail_pending(self, reason: str) -> None:
        with self.pending_lock:
            self.failure = reason
            pending, self.pending = self.pending, {}
            dispatch, self.dispatch = self.dispatch, {}
        for request_id, response in pending.items():
            state = dispatch.get(request_id, "completed_or_unknown")
            response.put_nowait({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32000, "message": reason, "data": {"dispatch_state": state, "execution_uncertain": state != "queued", "automatic_retry": False}}})

    def read_responses(self) -> None:
        assert self.child is not None and self.child.stdout is not None
        try:
            while True:
                message = _read_frame(self.child.stdout)
                request_id = message.get("id")
                if "method" in message:
                    if request_id is not None:
                        self.send({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32601, "message": "bridge client does not advertise server-request capabilities"}})
                    continue
                with self.pending_lock:
                    response = self.pending.pop(request_id, None)
                    self.dispatch.pop(request_id, None)
                if response is not None:
                    response.put_nowait(message)
        except (BridgeError, OSError) as error:
            self.fail_pending(f"MCP transport closed: {error}")
            self.stopping.set()

    def cancel(self, request_id: int) -> str:
        with self.pending_lock:
            self.pending.pop(request_id, None)
            dispatch = self.dispatch.pop(request_id, "completed_or_unknown")
        if dispatch == "queued":
            return "cancelled_before_send"
        try:
            self.send({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": request_id, "reason": "local client disconnected or timed out"}})
        except (OSError, BridgeError):
            pass
        return dispatch

    def serve_client(self, connection: socket.socket) -> None:
        request_id: int | None = None
        cancel_on_disconnect = True
        stop_after_reply = False
        try:
            connection.settimeout(STARTUP_TIMEOUT)
            with connection.makefile("rb") as stream:
                incoming = _read_frame(stream)
            method = incoming.get("method")
            params = incoming.get("params", {})
            timeout = _positive_timeout(incoming.get("timeout", 600))
            if not isinstance(method, str) or not isinstance(params, dict):
                raise BridgeError("method must be a string and params an object")
            if method in ("bridge/status", "bridge/stop"):
                assert self.child is not None
                result = {"running": self.child.poll() is None, "bridge_pid": os.getpid(), "mcp_pid": self.child.pid, "binary": str(self.config.binary), "root": str(self.config.root), "backends": str(self.config.backends), "socket": str(self.config.socket_path), "started_at": self.started_at, "stopping": method == "bridge/stop"}
                response = {"result": result}
                stop_after_reply = method == "bridge/stop"
            else:
                if method not in ("tools/call", "tools/list", "resources/list", "resources/read", "prompts/list", "prompts/get", "ping"):
                    raise BridgeError(f"unsupported bridge method: {method}")
                arguments = params.get("arguments", {})
                cancel_on_disconnect = not (
                    method == "tools/call"
                    and params.get("name") in ("o_cli", "o_eval")
                    and isinstance(arguments, dict)
                    and arguments.get("background") is True
                )
                request_id, responses = self.submit(method, params)
                deadline = time.monotonic() + timeout if timeout else None
                while True:
                    remaining = deadline - time.monotonic() if deadline is not None else None
                    if remaining is not None and remaining <= 0:
                        raise BridgeError(f"MCP request timed out after {timeout:g}s")
                    try:
                        response = responses.get(timeout=min(remaining, 0.1) if remaining is not None else 0.1)
                        request_id = None
                        break
                    except queue.Empty:
                        readable, _, _ = select.select([connection], [], [], 0)
                        if readable and not connection.recv(1, socket.MSG_PEEK):
                            return
            connection.sendall(_json_line(response))
        except (BridgeError, OSError, ValueError) as error:
            data: dict[str, Any] = {"automatic_retry": False}
            if request_id is not None and cancel_on_disconnect:
                data["dispatch_state"] = self.cancel(request_id)
                data["execution_uncertain"] = data["dispatch_state"] != "cancelled_before_send"
                request_id = None
            try:
                connection.sendall(_json_line({"error": {"code": -32000, "message": str(error), "data": data}}))
            except OSError:
                pass
        finally:
            if request_id is not None:
                if cancel_on_disconnect:
                    self.cancel(request_id)
                else:
                    with self.pending_lock:
                        self.pending.pop(request_id, None)
            connection.close()
            if stop_after_reply:
                self.stopping.set()

    def run(self) -> None:
        self.config.private_directory()
        os.umask(0o077)
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        bound_identity: tuple[int, int] | None = None
        writer: threading.Thread | None = None
        previous_handlers = {
            signum: signal.signal(signum, lambda _number, _frame: self.stopping.set())
            for signum in (signal.SIGTERM, signal.SIGINT)
        }
        with _private_file(self.config.directory / f"{self.config.key}.stderr.log") as stderr:
            try:
                self.child = subprocess.Popen([str(self.config.binary)], cwd=self.config.root, env=self.config.environment(), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, bufsize=0)
                assert self.child.stdin is not None
                os.set_blocking(self.child.stdin.fileno(), False)
                writer = threading.Thread(target=self.write_requests, daemon=True, name="mcp-stdin")
                writer.start()
                threading.Thread(target=self.read_responses, daemon=True, name="mcp-stdout").start()
                _, initialized = self.submit("initialize", {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "ostadix-agent-bridge", "version": "1"}})
                deadline = time.monotonic() + STARTUP_TIMEOUT - 2
                while True:
                    if self.stopping.is_set():
                        raise BridgeError("MCP bridge stopped during initialize")
                    if time.monotonic() >= deadline:
                        raise BridgeError("MCP initialize timed out")
                    try:
                        response = initialized.get(timeout=0.1)
                        break
                    except queue.Empty:
                        continue
                if "error" in response or not isinstance(response.get("result"), dict):
                    raise BridgeError(f"MCP initialize failed: {response}")
                self.send({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
                listener.bind(str(self.config.socket_path))
                metadata = self.config.socket_path.lstat()
                bound_identity = (metadata.st_dev, metadata.st_ino)
                listener.listen(128)
                listener.settimeout(0.25)
                while not self.stopping.is_set():
                    try:
                        connection, _ = listener.accept()
                    except socket.timeout:
                        continue
                    threading.Thread(target=self.serve_client, args=(connection,), daemon=True).start()
            finally:
                self.stopping.set()
                listener.close()
                self.fail_pending("MCP bridge stopped")
                if writer is not None:
                    writer.join(timeout=1)
                if self.child is not None:
                    if self.child.stdin is not None:
                        self.child.stdin.close()
                    try:
                        self.child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        self.child.terminate()
                        try:
                            self.child.wait(timeout=2)
                        except subprocess.TimeoutExpired:
                            self.child.kill()
                            self.child.wait()
                    if self.child.stdout is not None:
                        self.child.stdout.close()
                if bound_identity is not None:
                    try:
                        metadata = self.config.socket_path.lstat()
                        if (metadata.st_dev, metadata.st_ino) == bound_identity:
                            self.config.socket_path.unlink()
                    except FileNotFoundError:
                        pass
                for signum, handler in previous_handlers.items():
                    signal.signal(signum, handler)


def _result(response: dict[str, Any]) -> dict[str, Any]:
    if "error" in response:
        raise BridgeError(json.dumps(response["error"], ensure_ascii=False))
    result = response.get("result")
    if not isinstance(result, dict):
        raise BridgeError("MCP response omitted an object result")
    return result


def _render_tool(result: dict[str, Any]) -> str:
    # Keep the full result for structured tools and all failures: isError must
    # never disappear when a CLI exits nonzero. Legacy successful text remains
    # convenient for existing shell/aichat function wrappers.
    if "structuredContent" in result or result.get("isError") is True:
        return json.dumps(result, ensure_ascii=False)
    texts = [block.get("text", "") for block in result.get("content", []) if isinstance(block, dict) and block.get("type") == "text"]
    return "\n".join(texts) if texts else json.dumps(result, ensure_ascii=False)


def _process_exists(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def _tool_timeout(arguments: dict[str, Any], override: float | None) -> float:
    if override is not None:
        return _positive_timeout(override)
    default = _positive_timeout(os.environ.get("OSTADIX_MCP_CLIENT_TIMEOUT", "600"))
    requested = arguments.get("timeout_secs")
    if isinstance(requested, (int, float)) and not isinstance(requested, bool) and math.isfinite(requested) and requested >= 0:
        if requested == 0 or default == 0:
            return 0.0
        return max(default, float(requested) + 30.0)
    return default


def call_tool(tool: str, arguments: dict[str, Any] | None, timeout: float | None = None) -> str:
    try:
        result = _result(request("tools/call", {"name": tool, "arguments": arguments or {}}, _tool_timeout(arguments or {}, timeout)))
        return _render_tool(result)
    except (BridgeError, OSError) as error:
        return json.dumps({"isError": True, "error": str(error)})


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group()
    action.add_argument("--list-tools", action="store_true")
    action.add_argument("--status", action="store_true")
    action.add_argument("--stop", action="store_true")
    action.add_argument("--serve", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--timeout", type=float)
    parser.add_argument("tool", nargs="?")
    parser.add_argument("arguments", nargs="?", default="{}")
    arguments = parser.parse_args(argv)
    try:
        config = Configuration.from_environment()
        if arguments.serve:
            StdioBridge(config).run()
            return 0
        if arguments.status or arguments.stop:
            method = "bridge/stop" if arguments.stop else "bridge/status"
            result = _result(request(method, config=config, start=False, timeout=10))
            if arguments.stop and result.get("running"):
                deadline = time.monotonic() + 10
                old_pids = (result["bridge_pid"], result["mcp_pid"])
                while any(_process_exists(pid) for pid in old_pids) and time.monotonic() < deadline:
                    time.sleep(0.025)
                if any(_process_exists(pid) for pid in old_pids):
                    raise BridgeError("MCP bridge did not finish shutdown within 10s")
                result.update({"running": False, "stopped": True})
            print(json.dumps(result, ensure_ascii=False))
            return 0
        if arguments.list_tools:
            print(json.dumps(_result(request("tools/list", timeout=_tool_timeout({}, arguments.timeout), config=config)), ensure_ascii=False))
            return 0
        if not arguments.tool:
            parser.error("provide TOOL JSON, --list-tools, --status, or --stop")
        value = json.loads(arguments.arguments)
        if not isinstance(value, dict):
            raise BridgeError("tool arguments must be a JSON object")
        # Some function callers send optional fields as null instead of omitting
        # them. Preserve the original adapter's top-level normalization.
        value = {key: item for key, item in value.items() if item is not None}
        result = _result(request("tools/call", {"name": arguments.tool, "arguments": value}, _tool_timeout(value, arguments.timeout), config=config))
        print(_render_tool(result))
        return 1 if result.get("isError") is True else 0
    except (BridgeError, OSError, ValueError) as error:
        print(json.dumps({"isError": True, "error": str(error)}, ensure_ascii=False), file=sys.stderr if arguments.serve else sys.stdout)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
