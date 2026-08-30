#!/usr/bin/env python3
"""Exercise the released ostadix-mcp server over its real stdio transport."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any, BinaryIO


PROTOCOL_VERSION = "2025-03-26"
EXPECTED_TOOLS = {
    "o_analyze_intent",
    "o_doctor",
    "o_env",
    "o_execute_intent",
    "o_information_inspect",
    "o_olangc",
    "o_run",
    "o_runtimes",
    "o_search_run",
    "o_smoke",
}


def _snapshot_tree(root: Path) -> tuple[tuple[Any, ...], ...]:
    entries: list[tuple[Any, ...]] = []
    for path in sorted(root.rglob("*")):
        metadata = path.lstat()
        digest = hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None
        entries.append(
            (
                path.relative_to(root).as_posix(),
                path.is_dir(),
                metadata.st_ino,
                metadata.st_mode,
                metadata.st_size,
                metadata.st_mtime_ns,
                digest,
            )
        )
    return tuple(entries)


class SmokeError(RuntimeError):
    """The MCP server did not satisfy its released transport contract."""


_EOF = object()


def _current_catalog_schema(root: Path) -> str:
    """Read the one authoritative catalog-generation identifier."""
    catalog = root / "crates" / "ostadix-api" / "src" / "backend_catalog.inc.rs"
    match = re.search(
        r'backend_catalog_metadata!\s*\{\s*current_schema:\s*"([^"]+)"',
        catalog.read_text(encoding="utf-8"),
        re.DOTALL,
    )
    if match is None:
        raise SmokeError(f"backend catalog does not declare current_schema: {catalog}")
    return match.group(1)


class ResponseReader:
    """Drain newline-framed MCP stdout continuously and retain replies by id."""

    def __init__(self, stream: BinaryIO) -> None:
        self._stream = stream
        self._frames: queue.Queue[bytes | BaseException | object] = queue.Queue()
        self._pending: dict[int, dict[str, Any]] = {}
        self._thread = threading.Thread(
            target=self._read_frames,
            name="ostadix-mcp-stdout",
            daemon=True,
        )
        self._thread.start()

    def _read_frames(self) -> None:
        try:
            while line := self._stream.readline():
                self._frames.put(line)
        except BaseException as error:  # surfaced synchronously by response()
            self._frames.put(error)
        finally:
            self._frames.put(_EOF)

    def response(self, request_id: int, timeout: float) -> dict[str, Any]:
        waiting = self._pending.pop(request_id, None)
        if waiting is not None:
            return _checked_result(waiting, request_id)

        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise SmokeError(f"timeout waiting for MCP response {request_id}")
            try:
                frame = self._frames.get(timeout=remaining)
            except queue.Empty as error:
                raise SmokeError(
                    f"timeout waiting for MCP response {request_id}"
                ) from error
            if frame is _EOF:
                raise SmokeError(
                    f"MCP stdout closed while waiting for response {request_id}"
                )
            if isinstance(frame, BaseException):
                raise SmokeError(f"failed reading MCP stdout: {frame}") from frame
            try:
                message = json.loads(frame.decode("utf-8", "strict"))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise SmokeError(f"invalid MCP JSON response: {frame!r}") from error
            if not isinstance(message, dict):
                raise SmokeError(f"MCP response is not a JSON object: {frame!r}")
            response_id = message.get("id")
            if response_id == request_id:
                return _checked_result(message, request_id)
            if isinstance(response_id, int):
                if response_id in self._pending:
                    raise SmokeError(f"duplicate MCP response id {response_id}")
                self._pending[response_id] = message

    def join(self, timeout: float) -> None:
        self._thread.join(timeout)
        if self._thread.is_alive():
            raise SmokeError("MCP stdout reader did not stop")


def _send(process: subprocess.Popen[bytes], message: dict[str, Any]) -> None:
    if process.stdin is None:
        raise SmokeError("MCP stdin is unavailable")
    process.stdin.write(
        json.dumps(message, sort_keys=True, separators=(",", ":")).encode("utf-8")
        + b"\n"
    )
    process.stdin.flush()


def _checked_result(message: dict[str, Any], request_id: int) -> dict[str, Any]:
    if "error" in message:
        raise SmokeError(f"MCP request {request_id} failed: {message['error']}")
    result = message.get("result")
    if not isinstance(result, dict):
        raise SmokeError(f"MCP response {request_id} has no object result")
    return result


def _content_text(result: dict[str, Any]) -> str:
    content = result.get("content")
    if not isinstance(content, list):
        raise SmokeError("MCP tool result has no content list")
    pieces = [
        item.get("text", "")
        for item in content
        if isinstance(item, dict) and item.get("type") == "text"
    ]
    if not pieces:
        raise SmokeError("MCP tool result has no text content")
    return "\n".join(pieces)


def _record_field(text: str, key: str) -> str:
    prefix = f"{key}="
    for line in text.splitlines():
        if line.startswith(prefix):
            value = line[len(prefix) :]
            if value:
                return value
    raise SmokeError(f"MCP result omitted nonempty {key}= record:\n{text}")


def run_smoke(
    root: Path,
    binary: Path,
    timeout: float,
    *,
    o_info: Path | None = None,
    runtime_bin_dir: Path | None = None,
    server_cwd: Path | None = None,
    require_wasm: bool = False,
    require_wasm_materialization: bool = False,
    wasm_release_manifest: Path | None = None,
    wasm_release_artifact: Path | None = None,
    wasm_source_tree: str | None = None,
    wasm_base_commit: str | None = None,
    wasm_source_archive_sha256: str | None = None,
    wasm_timeout: float = 900.0,
) -> dict[str, Any] | None:
    catalog_schema = _current_catalog_schema(root)
    config = json.loads((root / ".mcp.json").read_text(encoding="utf-8"))
    registered = config.get("mcpServers", {}).get("ostadix", {})
    if registered.get("command") != "ostadix-mcp":
        raise SmokeError(".mcp.json does not register the released ostadix-mcp command")
    if registered.get("args") != []:
        raise SmokeError(".mcp.json must register ostadix-mcp with an empty argv")
    if "env" in registered:
        raise SmokeError(
            ".mcp.json must not rely on client-specific shell expansion in environment values"
        )

    environment = os.environ.copy()
    environment.pop("O_LANG_ROOT", None)
    environment.pop("O_BACKENDS_DIR", None)
    environment.pop("OLANG", None)
    environment.pop("OSTADIX_RUNTIME_PATH", None)
    environment.pop("OSTADIX_O_INFO_BIN", None)
    environment.pop("A18_WORK", None)
    environment["OSTADIX_RUNTIME_PATH_MODE"] = "discover-local"
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    # Model the restricted environment used by GUI-launched MCP clients. The
    # server must restore local runtime locations without shell startup files.
    restricted_path = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
    if runtime_bin_dir is not None:
        if (
            not runtime_bin_dir.is_absolute()
            or runtime_bin_dir.is_symlink()
            or not runtime_bin_dir.is_dir()
        ):
            raise SmokeError(
                "installed runtime bin directory is not an absolute "
                f"non-symlink directory: {runtime_bin_dir}"
            )
        restricted_path.insert(0, os.fspath(runtime_bin_dir))
    environment["PATH"] = os.pathsep.join(restricted_path)
    environment["RUST_LOG"] = "warn"
    launch_cwd = server_cwd if server_cwd is not None else root
    if not launch_cwd.is_absolute() or launch_cwd.is_symlink() or not launch_cwd.is_dir():
        raise SmokeError(
            "MCP launch directory is not an absolute non-symlink directory: "
            f"{launch_cwd}"
        )
    if not (1.0 <= wasm_timeout <= 1800.0):
        raise SmokeError("WASM timeout must be from 1 through 1800 seconds")
    if require_wasm and require_wasm_materialization:
        raise SmokeError("fresh WASM compilation and materialization are mutually exclusive")
    if require_wasm_materialization:
        if server_cwd is None:
            raise SmokeError("WASM materialization requires an explicit MCP server cwd")
        required_release_values = {
            "manifest": wasm_release_manifest,
            "artifact": wasm_release_artifact,
            "source tree": wasm_source_tree,
            "base commit": wasm_base_commit,
            "source archive SHA-256": wasm_source_archive_sha256,
        }
        missing = [label for label, value in required_release_values.items() if value is None]
        if missing:
            raise SmokeError(
                "WASM materialization omitted release bindings: " + ", ".join(missing)
            )
        for label, path in (
            ("WASM release manifest", wasm_release_manifest),
            ("WASM release artifact", wasm_release_artifact),
        ):
            assert path is not None
            if not path.is_absolute() or path.is_symlink() or not path.is_file():
                raise SmokeError(f"{label} is not an absolute regular non-symlink file: {path}")

    stderr_capture = tempfile.TemporaryFile()
    home_fixture = tempfile.TemporaryDirectory(prefix=".mcp-home-smoke-")
    environment["HOME"] = home_fixture.name
    intent_fixture = tempfile.TemporaryDirectory(prefix=".mcp-intent-smoke-")
    information_fixture = tempfile.TemporaryDirectory(prefix=".mcp-information-smoke-")
    wasm_fixture = (
        tempfile.TemporaryDirectory(prefix=".mcp-wasm-smoke-")
        if require_wasm
        else None
    )
    wasm_output = (
        Path(wasm_fixture.name) / "ostadix-mcp-hello.wasm"
        if wasm_fixture is not None
        else None
    )
    materialize_fixture = (
        tempfile.TemporaryDirectory(
            prefix=".mcp-wasm-materialize-", dir=os.fspath(launch_cwd)
        )
        if require_wasm_materialization
        else None
    )
    materialize_project = (
        Path(materialize_fixture.name) / "generated"
        if materialize_fixture is not None
        else None
    )
    materialize_output = (
        Path(materialize_fixture.name) / "hello.wasm"
        if materialize_fixture is not None
        else None
    )
    wasm_materialization_evidence: dict[str, Any] | None = None
    information_state = Path(information_fixture.name) / "state"
    information_binary = o_info if o_info is not None else root / "target/release/o-info"
    if (
        not information_binary.is_absolute()
        or information_binary.is_symlink()
        or not information_binary.is_file()
    ):
        raise SmokeError(
            "fixed local o-info binary is not an absolute non-symlink file: "
            f"{information_binary}"
        )
    if o_info is not None:
        environment["OSTADIX_O_INFO_BIN"] = os.fspath(information_binary)
    initialized_information = subprocess.run(
        [os.fspath(information_binary), "init", "--state", os.fspath(information_state)],
        cwd=root,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if initialized_information.returncode != 0:
        raise SmokeError(
            "could not initialize MCP information smoke state: "
            + initialized_information.stderr.decode("utf-8", "replace")
        )
    information_before = _snapshot_tree(information_state)
    intent_program = Path(intent_fixture.name) / "intent.O"
    intent_marker = Path(intent_fixture.name) / "executed.marker"

    def write_intent_fixture(label: str) -> None:
        intent_program.write_text(
            "python^(\n"
            "from pathlib import Path\n"
            f"Path({json.dumps(os.fspath(intent_marker))}).write_text({label!r})\n"
            f"__oval_result__ = {label!r}\n"
            ")_python\n",
            encoding="utf-8",
        )

    write_intent_fixture("intent-original")
    process = subprocess.Popen(
        [os.fspath(binary)],
        cwd=launch_cwd,
        env=environment,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=stderr_capture,
        bufsize=0,
    )
    if process.stdout is None:
        process.kill()
        process.wait()
        stderr_capture.close()
        raise SmokeError("MCP stdout is unavailable")
    responses = ResponseReader(process.stdout)
    try:
        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "ostadix-release-smoke", "version": "1"},
                },
            },
        )
        initialized = responses.response(1, timeout)
        if initialized.get("protocolVersion") != PROTOCOL_VERSION:
            raise SmokeError("MCP initialize negotiated an unexpected protocol version")
        if "tools" not in initialized.get("capabilities", {}):
            raise SmokeError("MCP initialize did not advertise tools")

        _send(
            process,
            {
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {},
            },
        )
        _send(
            process,
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
        )
        listed = responses.response(2, timeout)
        tools = listed.get("tools")
        if not isinstance(tools, list):
            raise SmokeError("tools/list did not return a tool list")
        names = {
            tool["name"]
            for tool in tools
            if isinstance(tool, dict) and isinstance(tool.get("name"), str)
        }
        if names != EXPECTED_TOOLS:
            raise SmokeError(
                f"unexpected MCP tool set: expected {sorted(EXPECTED_TOOLS)}, got {sorted(names)}"
            )
        for tool in tools:
            if not isinstance(tool, dict):
                raise SmokeError(f"tools/list returned a non-object tool: {tool!r}")
            schema = tool.get("inputSchema")
            if (
                not isinstance(schema, dict)
                or schema.get("type") != "object"
                or not isinstance(schema.get("properties"), dict)
            ):
                raise SmokeError(
                    f"{tool.get('name', '<unnamed>')} has a non-object input schema: "
                    f"{schema!r}"
                )
        olangc_tools = [tool for tool in tools if tool.get("name") == "o_olangc"]
        if len(olangc_tools) != 1 or "materialize_only" not in olangc_tools[0][
            "inputSchema"
        ]["properties"]:
            raise SmokeError("o_olangc schema omitted materialize_only")

        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "o_env", "arguments": {}},
            },
        )
        environment_result = responses.response(3, timeout)
        if environment_result.get("isError") is True:
            raise SmokeError("o_env returned an MCP tool error")
        environment_text = _content_text(environment_result)
        required_environment = {
            f"O_LANG_ROOT={root}",
            f"O_BACKENDS_DIR={root / 'backends'}",
        }
        if not all(value in environment_text for value in required_environment):
            raise SmokeError(f"o_env returned unexpected paths:\n{environment_text}")
        if "runtime-summary backend-count=30" not in environment_text:
            raise SmokeError(f"o_env omitted the all-runtime summary:\n{environment_text}")

        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {"name": "o_runtimes", "arguments": {}},
            },
        )
        runtimes_result = responses.response(4, timeout)
        runtimes_text = _content_text(runtimes_result)
        if runtimes_result.get("isError") is True:
            raise SmokeError(f"o_runtimes returned an MCP tool error:\n{runtimes_text}")
        required_runtime_markers = {
            f"runtime-catalog-schema={catalog_schema}",
            "runtime-catalog-legacy-schema-v5=ostadix.backend-catalog/v5",
            "runtime-catalog-legacy-schema-v4=ostadix.backend-catalog/v4",
            "runtime-catalog-projection=compiled-mcp-snapshot",
            "runtime-search-mode=discover-local",
            "runtime-summary backend-count=30",
            "runtime backends=python status=located",
            "runtime backends=java status=",
            "runtime backends=webassembly status=",
            "precision=conservative-all-sources",
            "invocable=not-probed",
            "admitted=operation-scoped-not-evaluated",
            "path-sources=[python3=",
            "backend=python integer-exactness=arbitrary rich-numbers=preserved "
            "state-support=semantic-snapshot codec=ostadix.python-graph/v1 "
            "compatibility=exact-implementation morphism-profile=python-plain-data",
            "backend=javascript integer-exactness=exact-magnitude-bits:53 "
            "rich-numbers=collapsed state-support=stateless "
            "morphism-profile=javascript-binding-stdout",
            "backend=html integer-exactness=arbitrary rich-numbers=collapsed "
            "state-support=stateless morphism-profile=none",
            "morphism profiles are bounded shadow descriptions; they do not authorize "
            "execution or claim generic backend crossings",
        }
        required_runtime_markers.update(
            f"runtime-search-entry index={index} source=inherited:{index} path={path}"
            for index, path in enumerate(restricted_path)
        )
        if not all(marker in runtimes_text for marker in required_runtime_markers):
            raise SmokeError(
                "o_runtimes omitted required backend discovery markers:\n"
                f"{runtimes_text}"
            )

        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {"name": "o_smoke", "arguments": {}},
            },
        )
        smoke_result = responses.response(5, timeout)
        smoke_text = _content_text(smoke_result)
        if smoke_result.get("isError") is True or "SMOKE_OK" not in smoke_text:
            raise SmokeError(f"o_smoke failed:\n{smoke_text}")
        if "[number] 2" not in smoke_text:
            raise SmokeError(f"o_smoke omitted the expected result 2:\n{smoke_text}")

        calls = [
            (
                6,
                "o_run",
                {"path": "examples/hello.O", "timeout_secs": 45},
                "[number] 2",
            ),
            (
                7,
                "o_run",
                {"path": "hello.O", "cwd": "examples", "timeout_secs": 45},
                "[number] 2",
            ),
            (
                8,
                "o_olangc",
                {"path": "examples/hello.O", "target": "ir", "timeout_secs": 45},
                "; OIrProgram",
            ),
        ]
        for request_id, tool, arguments, marker in calls:
            _send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": "tools/call",
                    "params": {"name": tool, "arguments": arguments},
                },
            )
            result = responses.response(request_id, timeout)
            result_text = _content_text(result)
            if result.get("isError") is True or marker not in result_text:
                raise SmokeError(
                    f"{tool} relative-path smoke failed; expected {marker!r}:\n"
                    f"{result_text}"
                )

        # Analyze is nonexecuting; mutation after analysis must be rejected by
        # O's recomputation, and the failed attempt must still consume the
        # handle so it cannot be replayed.
        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/call",
                "params": {
                    "name": "o_analyze_intent",
                    "arguments": {
                        "path": os.fspath(intent_program),
                        "ttl_secs": 60,
                        "timeout_secs": 45,
                    },
                },
            },
        )
        analyzed = responses.response(9, timeout)
        analyzed_text = _content_text(analyzed)
        if analyzed.get("isError") is True:
            raise SmokeError(f"o_analyze_intent failed:\n{analyzed_text}")
        handle = _record_field(analyzed_text, "intent-handle")
        if "intent-schema=oexec.execution-intent/v1" not in analyzed_text:
            raise SmokeError(
                f"o_analyze_intent omitted the stable schema:\n{analyzed_text}"
            )
        if len(_record_field(analyzed_text, "source-sha256")) != 64:
            raise SmokeError(f"o_analyze_intent emitted a bad source digest:\n{analyzed_text}")
        if intent_marker.exists():
            raise SmokeError("o_analyze_intent executed the inspected Python backend")

        write_intent_fixture("intent-mutated")
        execute_arguments = {
            "handle": handle,
            "path": os.fspath(intent_program),
            "timeout_secs": 45,
        }
        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 10,
                "method": "tools/call",
                "params": {
                    "name": "o_execute_intent",
                    "arguments": execute_arguments,
                },
            },
        )
        mutated = responses.response(10, timeout)
        mutated_text = _content_text(mutated)
        if mutated.get("isError") is not True or not (
            "source" in mutated_text.lower() and "mismatch" in mutated_text.lower()
        ):
            raise SmokeError(
                "o_execute_intent did not reject source mutation with a source mismatch:\n"
                f"{mutated_text}"
            )
        if intent_marker.exists():
            raise SmokeError("rejected source mutation dispatched the Python backend")

        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 11,
                "method": "tools/call",
                "params": {
                    "name": "o_execute_intent",
                    "arguments": execute_arguments,
                },
            },
        )
        replay = responses.response(11, timeout)
        replay_text = _content_text(replay)
        if replay.get("isError") is not True or "already-consumed" not in replay_text:
            raise SmokeError(f"consumed intent handle was replayable:\n{replay_text}")

        # A fresh handle over the mutated source succeeds exactly once.
        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 12,
                "method": "tools/call",
                "params": {
                    "name": "o_analyze_intent",
                    "arguments": {
                        "path": os.fspath(intent_program),
                        "timeout_secs": 45,
                    },
                },
            },
        )
        fresh = responses.response(12, timeout)
        fresh_text = _content_text(fresh)
        if fresh.get("isError") is True:
            raise SmokeError(f"fresh o_analyze_intent failed:\n{fresh_text}")
        fresh_handle = _record_field(fresh_text, "intent-handle")
        fresh_arguments = {
            "handle": fresh_handle,
            "path": os.fspath(intent_program),
            "timeout_secs": 45,
        }
        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 13,
                "method": "tools/call",
                "params": {
                    "name": "o_execute_intent",
                    "arguments": fresh_arguments,
                },
            },
        )
        executed = responses.response(13, timeout)
        executed_text = _content_text(executed)
        if (
            executed.get("isError") is True
            or "intent-consumed=true" not in executed_text
            or "intent-mutated" not in executed_text
        ):
            raise SmokeError(f"fresh intent execution failed:\n{executed_text}")
        if intent_marker.read_text(encoding="utf-8") != "intent-mutated":
            raise SmokeError("matching intent did not commit the expected backend effect")
        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 14,
                "method": "tools/call",
                "params": {
                    "name": "o_execute_intent",
                    "arguments": fresh_arguments,
                },
            },
        )
        successful_replay = responses.response(14, timeout)
        successful_replay_text = _content_text(successful_replay)
        if (
            successful_replay.get("isError") is not True
            or "already-consumed" not in successful_replay_text
        ):
            raise SmokeError(
                f"successfully consumed intent handle was replayable:\n{successful_replay_text}"
            )

        # Echoed target arguments are part of the handle binding. A mismatch
        # is rejected before O starts and consumes the attempted handle.
        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 15,
                "method": "tools/call",
                "params": {
                    "name": "o_analyze_intent",
                    "arguments": {"path": os.fspath(intent_program)},
                },
            },
        )
        mismatch_analysis = responses.response(15, timeout)
        mismatch_text = _content_text(mismatch_analysis)
        mismatch_handle = _record_field(mismatch_text, "intent-handle")
        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 16,
                "method": "tools/call",
                "params": {
                    "name": "o_execute_intent",
                    "arguments": {
                        "handle": mismatch_handle,
                        "path": "examples/hello.O",
                    },
                },
            },
        )
        mismatched = responses.response(16, timeout)
        mismatched_text = _content_text(mismatched)
        if mismatched.get("isError") is not True or "program mismatch" not in mismatched_text:
            raise SmokeError(f"intent target mismatch was accepted:\n{mismatched_text}")
        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 17,
                "method": "tools/call",
                "params": {
                    "name": "o_execute_intent",
                    "arguments": {
                        "handle": mismatch_handle,
                        "path": os.fspath(intent_program),
                    },
                },
            },
        )
        mismatch_replay = responses.response(17, timeout)
        mismatch_replay_text = _content_text(mismatch_replay)
        if (
            mismatch_replay.get("isError") is not True
            or "already-consumed" not in mismatch_replay_text
        ):
            raise SmokeError(
                f"mismatched intent attempt did not consume its handle:\n{mismatch_replay_text}"
            )

        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 18,
                "method": "tools/call",
                "params": {
                    "name": "o_information_inspect",
                    "arguments": {
                        "state": os.fspath(information_state),
                        "head": "main",
                        "timeout_secs": 10,
                    },
                },
            },
        )
        information_result = responses.response(18, timeout)
        information_text = _content_text(information_result)
        if information_result.get("isError") is True:
            raise SmokeError(
                f"o_information_inspect failed on initialized local state:\n{information_text}"
            )
        required_information = {
            "head=main",
            "facts=0",
            "authority=information presence and signatures grant no execution authority",
            "source=local-o-info-read-only",
        }
        if not all(marker in information_text for marker in required_information):
            raise SmokeError(
                f"o_information_inspect omitted bounded records:\n{information_text}"
            )
        if os.fspath(information_state) in information_text or "state=" in information_text:
            raise SmokeError(
                f"o_information_inspect leaked its request-local state path:\n{information_text}"
            )
        if _snapshot_tree(information_state) != information_before:
            raise SmokeError("o_information_inspect mutated the local information store")

        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 19,
                "method": "tools/call",
                "params": {
                    "name": "o_information_inspect",
                    "arguments": {
                        "state": os.fspath(information_state),
                        "head": "../main",
                    },
                },
            },
        )
        invalid_information = responses.response(19, timeout)
        if invalid_information.get("isError") is not True:
            raise SmokeError("o_information_inspect accepted a non-token head name")
        if _snapshot_tree(information_state) != information_before:
            raise SmokeError("rejected information inspection mutated the local store")

        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 20,
                "method": "tools/call",
                "params": {"name": "o_doctor", "arguments": {}},
            },
        )
        doctor_result = responses.response(20, timeout)
        doctor_text = _content_text(doctor_result)
        required_doctor = {
            f"O_LANG_ROOT={root} exists=true",
            f"search-work={root}",
            f"search-corpus={root / 'examples'} bundled=true",
        }
        if doctor_result.get("isError") is True or not all(
            marker in doctor_text for marker in required_doctor
        ):
            raise SmokeError(f"o_doctor omitted installed-layout records:\n{doctor_text}")

        _send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 21,
                "method": "tools/call",
                "params": {
                    "name": "o_search_run",
                    "arguments": {"name": "hello", "timeout_secs": 45},
                },
            },
        )
        search_result = responses.response(21, timeout)
        search_text = _content_text(search_result)
        required_search = {
            f"program={root / 'examples/hello.O'}",
            f"corpus={root / 'examples'}",
            "[number] 2",
        }
        if search_result.get("isError") is True or not all(
            marker in search_text for marker in required_search
        ):
            raise SmokeError(f"o_search_run bundled corpus failed:\n{search_text}")

        for request_id, rejected_name in (
            (22, "../hello"),
            (23, os.fspath(root / "examples/hello.O")),
        ):
            _send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": "tools/call",
                    "params": {
                        "name": "o_search_run",
                        "arguments": {"name": rejected_name},
                    },
                },
            )
            rejected_search = responses.response(request_id, timeout)
            rejected_text = _content_text(rejected_search)
            if (
                rejected_search.get("isError") is not True
                or "leaf token" not in rejected_text
            ):
                raise SmokeError(
                    "o_search_run accepted a path outside its leaf-token contract:\n"
                    f"{rejected_text}"
                )

        if require_wasm:
            assert wasm_output is not None
            _send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": 24,
                    "method": "tools/call",
                    "params": {
                        "name": "o_olangc",
                        "arguments": {
                            "path": "examples/wasm_hello.O",
                            "target": "wasm",
                            "output": os.fspath(wasm_output),
                            "timeout_secs": int(wasm_timeout),
                        },
                    },
                },
            )
            wasm_result = responses.response(24, wasm_timeout + 30.0)
            wasm_text = _content_text(wasm_result)
            if wasm_result.get("isError") is True or "exit=0" not in wasm_text:
                raise SmokeError(f"o_olangc WASM compile failed:\n{wasm_text}")
            if (
                wasm_output.is_symlink()
                or not wasm_output.is_file()
                or wasm_output.read_bytes()[:4] != b"\x00asm"
            ):
                raise SmokeError(
                    "o_olangc WASM compile omitted a regular WebAssembly artifact"
                )
        elif require_wasm_materialization:
            assert materialize_project is not None
            assert materialize_output is not None
            _send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": 24,
                    "method": "tools/call",
                    "params": {
                        "name": "o_olangc",
                        "arguments": {
                            "path": "examples/wasm_hello.O",
                            "target": "wasm",
                            "output": os.fspath(materialize_output),
                            "materialize_only": os.fspath(materialize_project),
                            "timeout_secs": int(timeout),
                        },
                    },
                },
            )
            materialize_result = responses.response(24, timeout + 30.0)
            materialize_text = _content_text(materialize_result)
            expected_record = (
                "olangc: materialize-only target=wasm rust-target=wasm32-wasip1 "
                f"cargo-invoked=false dir={materialize_project}"
            )
            if (
                materialize_result.get("isError") is True
                or "exit=0" not in materialize_text
                or expected_record not in materialize_text
            ):
                raise SmokeError(
                    f"o_olangc WASM materialization failed:\n{materialize_text}"
                )
            required_project_files = (
                "Cargo.toml",
                "Cargo.lock",
                "rust-toolchain.toml",
                "src/lib.rs",
                "src/main.rs",
                "src/program.O",
            )
            if (
                materialize_project.is_symlink()
                or not materialize_project.is_dir()
                or any(
                    not (materialize_project / relative).is_file()
                    for relative in required_project_files
                )
                or (materialize_project / "target").exists()
                or materialize_output.exists()
                or (materialize_project / "src/program.O").read_bytes()
                != (root / "examples/wasm_hello.O").read_bytes()
            ):
                raise SmokeError(
                    "o_olangc materialization omitted or mutated its exact no-build project"
                )
            assert wasm_release_manifest is not None
            assert wasm_release_artifact is not None
            assert wasm_source_tree is not None
            assert wasm_base_commit is not None
            assert wasm_source_archive_sha256 is not None
            generator = (
                runtime_bin_dir / "olangc"
                if runtime_bin_dir is not None
                else root / "target/release/olangc"
            )
            verifier = root / "scripts/ostadix_wasm_release.py"
            verified = subprocess.run(
                [
                    sys.executable,
                    os.fspath(verifier),
                    "verify",
                    "--manifest",
                    os.fspath(wasm_release_manifest),
                    "--project",
                    os.fspath(materialize_project),
                    "--artifact",
                    os.fspath(wasm_release_artifact),
                    "--input",
                    os.fspath(root / "examples/wasm_hello.O"),
                    "--generator",
                    os.fspath(generator),
                    "--source-tree",
                    wasm_source_tree,
                    "--base-commit",
                    wasm_base_commit,
                    "--source-archive-sha256",
                    wasm_source_archive_sha256,
                ],
                cwd=launch_cwd,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=timeout,
                check=False,
            )
            if verified.returncode != 0:
                raise SmokeError(
                    "MCP-materialized WASM project failed its release binding:\n"
                    + verified.stderr.decode("utf-8", "replace")
                )
            try:
                wasm_materialization_evidence = json.loads(
                    verified.stdout.decode("utf-8")
                )
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise SmokeError(
                    "WASM release verifier returned malformed JSON"
                ) from error
    finally:
        if process.stdin is not None:
            try:
                process.stdin.close()
            except OSError:
                pass
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            raise SmokeError("ostadix-mcp did not exit after stdin closed")
        responses.join(timeout)
        stderr_capture.seek(0)
        stderr = stderr_capture.read().decode("utf-8", "replace")
        stderr_capture.close()
        intent_fixture.cleanup()
        information_fixture.cleanup()
        if wasm_fixture is not None:
            wasm_fixture.cleanup()
        if materialize_fixture is not None:
            materialize_fixture.cleanup()
        home_fixture.cleanup()
        if process.returncode != 0:
            raise SmokeError(
                f"ostadix-mcp exited {process.returncode}; stderr:\n{stderr}"
            )
    return wasm_materialization_evidence


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--o-info", type=Path)
    parser.add_argument("--runtime-bin-dir", type=Path)
    parser.add_argument("--server-cwd", type=Path)
    wasm_mode = parser.add_mutually_exclusive_group()
    wasm_mode.add_argument("--require-wasm", action="store_true")
    wasm_mode.add_argument("--require-wasm-materialization", action="store_true")
    parser.add_argument("--wasm-release-manifest", type=Path)
    parser.add_argument("--wasm-release-artifact", type=Path)
    parser.add_argument("--wasm-source-tree")
    parser.add_argument("--wasm-base-commit")
    parser.add_argument("--wasm-source-archive-sha256")
    parser.add_argument("--wasm-timeout", type=float, default=900.0)
    parser.add_argument("--timeout", type=float, default=120.0)
    arguments = parser.parse_args()

    root = arguments.root.expanduser().resolve()
    binary = (
        arguments.binary.expanduser().resolve()
        if arguments.binary
        else root / "mcp/ostadix_lang_mcp_server/target/release/ostadix-mcp"
    )
    if not binary.is_file():
        print(f"error: MCP binary not found: {binary}", file=sys.stderr)
        return 2
    o_info = arguments.o_info.expanduser() if arguments.o_info else None
    runtime_bin_dir = (
        arguments.runtime_bin_dir.expanduser()
        if arguments.runtime_bin_dir
        else None
    )
    server_cwd = (
        arguments.server_cwd.expanduser().resolve()
        if arguments.server_cwd
        else None
    )
    try:
        wasm_evidence = run_smoke(
            root,
            binary,
            arguments.timeout,
            o_info=o_info,
            runtime_bin_dir=runtime_bin_dir,
            server_cwd=server_cwd,
            require_wasm=arguments.require_wasm,
            require_wasm_materialization=arguments.require_wasm_materialization,
            wasm_release_manifest=arguments.wasm_release_manifest,
            wasm_release_artifact=arguments.wasm_release_artifact,
            wasm_source_tree=arguments.wasm_source_tree,
            wasm_base_commit=arguments.wasm_base_commit,
            wasm_source_archive_sha256=arguments.wasm_source_archive_sha256,
            wasm_timeout=arguments.wasm_timeout,
        )
    except (OSError, SmokeError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    if arguments.require_wasm:
        print("ostadix-mcp o_olangc wasm: PASS")
    if arguments.require_wasm_materialization:
        assert wasm_evidence is not None
        project = wasm_evidence["project"]
        source = wasm_evidence["source"]
        artifact = wasm_evidence["artifact"]
        print(
            "ostadix-mcp o_olangc wasm materialization: PASS "
            f"root_sha256={project['root_sha256']}"
        )
        print(
            "ostadix-mcp o_olangc wasm artifact: PASS "
            f"tree={source['staged_tree']} bytes={artifact['bytes']} "
            f"sha256={artifact['sha256']}"
        )
    print("ostadix-mcp stdio release smoke: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
