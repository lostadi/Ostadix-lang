#!/usr/bin/env python3
"""Shared helpers for backend shims.

Backend stdout is an O boundary. A script that prints a scalar or JSON value
should hand that value back to O as data, while arbitrary text remains text.
Languages can print a tagged OValue JSON envelope for exact control.
"""

import json
import math
import os
import re
import stat
import struct
import sys


INT64_MIN = -(2**63)
INT64_MAX = 2**63 - 1
MAX_FRAME_LEN = 128 * 1024 * 1024
INT_RE = re.compile(r"^[+-]?\d+$")
FLOAT_RE = re.compile(
    r"^[+-]?(?:(?:\d+\.\d*)|(?:\.\d+)|(?:\d+[eE][+-]?\d+)|(?:\d+\.\d*[eE][+-]?\d+))$"
)


def _identity_from_stat(result):
    return {
        "device": result.st_dev,
        "inode": result.st_ino,
        "size": result.st_size,
        "mode": result.st_mode,
        "mtime_seconds": result.st_mtime_ns // 1_000_000_000,
        "mtime_nanoseconds": result.st_mtime_ns % 1_000_000_000,
        "ctime_seconds": result.st_ctime_ns // 1_000_000_000,
        "ctime_nanoseconds": result.st_ctime_ns % 1_000_000_000,
    }


def admitted_tool_path(logical_command):
    """Revalidate and return one adapter-owned admitted direct launcher.

    Adapter-owned subprocesses must not resolve their own copies through PATH:
    the Rust proxy validates this backend-scoped manifest at startup, and this
    helper cheaply rechecks invocation and target identity immediately before
    every adapter-owned subprocess. User code remains free to launch its own
    commands; this helper is only for tools that are part of the adapter.
    """
    raw = os.environ.get("O_ADMITTED_EXECUTABLE_MANIFEST")
    if not raw:
        raise RuntimeError(
            f"adapter tool {logical_command!r} has no admitted executable manifest"
        )
    try:
        manifest = json.loads(raw)
    except (TypeError, ValueError) as exc:
        raise RuntimeError("admitted executable manifest is invalid JSON") from exc
    if manifest.get("schema") != "oexec.direct-executable-manifest/v1":
        raise RuntimeError("admitted executable manifest has an unsupported schema")
    matches = [
        artifact
        for artifact in manifest.get("artifacts", [])
        if artifact.get("role") == "direct-launcher"
        and artifact.get("logical_command") == logical_command
    ]
    if len(matches) != 1:
        raise RuntimeError(
            f"adapter tool {logical_command!r} has {len(matches)} admitted launchers; expected one"
        )
    artifact = matches[0]
    invocation_path = artifact.get("invocation_path")
    if not isinstance(invocation_path, str) or not os.path.isabs(invocation_path):
        raise RuntimeError(
            f"adapter tool {logical_command!r} has no admitted absolute invocation path"
        )
    canonical_path = artifact.get("canonical_path")
    if not isinstance(canonical_path, str) or not os.path.isabs(canonical_path):
        raise RuntimeError(
            f"adapter tool {logical_command!r} has no admitted canonical path"
        )
    expected_invocation = artifact.get("invocation_file_identity")
    expected_target = artifact.get("file_identity")
    if not isinstance(expected_invocation, dict) or not isinstance(expected_target, dict):
        raise RuntimeError(
            f"adapter tool {logical_command!r} has incomplete admitted file identity"
        )
    try:
        invocation_stat = os.lstat(invocation_path)
        target_stat = os.stat(canonical_path)
        resolved_path = os.path.realpath(invocation_path)
    except OSError as exc:
        raise RuntimeError(
            f"adapter tool {logical_command!r} cannot be revalidated: {exc}"
        ) from exc
    if not stat.S_ISREG(target_stat.st_mode):
        raise RuntimeError(f"adapter tool {logical_command!r} is no longer a regular file")
    if os.path.normcase(resolved_path) != os.path.normcase(canonical_path):
        raise RuntimeError(f"adapter tool {logical_command!r} resolves to a different target")
    if os.name == "posix":
        if _identity_from_stat(invocation_stat) != expected_invocation:
            raise RuntimeError(f"adapter tool {logical_command!r} invocation path changed")
        if _identity_from_stat(target_stat) != expected_target:
            raise RuntimeError(f"adapter tool {logical_command!r} target changed")
    else:
        # Rust's portable identity deliberately contains only size and mtime;
        # Windows reports different device/inode/mode/ctime values through
        # Python. Mirror that inexpensive per-use contract. The parent already
        # content-hashed the artifact at capture/proxy startup; re-hashing a
        # potentially large runtime before every adapter subprocess would turn
        # hardening into a capacity regression.
        actual_invocation = _identity_from_stat(invocation_stat)
        actual_target = _identity_from_stat(target_stat)
        for field in ("size", "mtime_seconds", "mtime_nanoseconds"):
            if actual_invocation[field] != expected_invocation.get(field):
                raise RuntimeError(f"adapter tool {logical_command!r} invocation path changed")
            if actual_target[field] != expected_target.get(field):
                raise RuntimeError(f"adapter tool {logical_command!r} target changed")
    return invocation_path


def _encode_type_len(major, length):
    prefix = major << 5
    if length <= 23:
        return bytes([prefix | length])
    if length <= 0xFF:
        return bytes([prefix | 24, length])
    if length <= 0xFFFF:
        return bytes([prefix | 25]) + length.to_bytes(2, "big")
    if length <= 0xFFFFFFFF:
        return bytes([prefix | 26]) + length.to_bytes(4, "big")
    return bytes([prefix | 27]) + length.to_bytes(8, "big")


def cbor_encode(value):
    if value is None:
        return b"\xf6"
    if value is False:
        return b"\xf4"
    if value is True:
        return b"\xf5"
    if isinstance(value, int):
        if value >= 0:
            return _encode_type_len(0, value)
        return _encode_type_len(1, -1 - value)
    if isinstance(value, float):
        return b"\xfb" + struct.pack(">d", value)
    if isinstance(value, bytes):
        return _encode_type_len(2, len(value)) + value
    if isinstance(value, str):
        encoded = value.encode("utf-8")
        return _encode_type_len(3, len(encoded)) + encoded
    if isinstance(value, (list, tuple)):
        return _encode_type_len(4, len(value)) + b"".join(cbor_encode(item) for item in value)
    if isinstance(value, dict):
        encoded_entries = []
        for key, item in value.items():
            encoded_key = cbor_encode(str(key))
            encoded_entries.append((encoded_key, cbor_encode(item)))
        encoded_entries.sort(key=lambda entry: (len(entry[0]), entry[0]))
        return (
            _encode_type_len(5, len(encoded_entries))
            + b"".join(key + item for key, item in encoded_entries)
        )
    raise TypeError(f"cannot encode {type(value).__name__} as O wire CBOR")


class _CborDecoder:
    def __init__(self, payload):
        self.payload = payload
        self.offset = 0

    def finish(self):
        if self.offset != len(self.payload):
            raise ValueError(f"CBOR payload has {len(self.payload) - self.offset} trailing bytes")

    def read(self, length):
        end = self.offset + length
        if end > len(self.payload):
            raise EOFError("unexpected end of CBOR payload")
        chunk = self.payload[self.offset:end]
        self.offset = end
        return chunk

    def read_u8(self):
        return self.read(1)[0]

    def read_len(self, additional):
        if additional <= 23:
            return additional
        if additional == 24:
            return self.read_u8()
        if additional == 25:
            return int.from_bytes(self.read(2), "big")
        if additional == 26:
            return int.from_bytes(self.read(4), "big")
        if additional == 27:
            return int.from_bytes(self.read(8), "big")
        if additional == 31:
            raise ValueError("indefinite-length CBOR is not allowed on the O wire")
        raise ValueError(f"invalid CBOR length discriminator {additional}")

    def decode(self):
        initial = self.read_u8()
        major = initial >> 5
        additional = initial & 0x1F

        if major == 0:
            return self.read_len(additional)
        if major == 1:
            return -1 - self.read_len(additional)
        if major == 2:
            return list(self.read(self.read_len(additional)))
        if major == 3:
            return self.read(self.read_len(additional)).decode("utf-8")
        if major == 4:
            return [self.decode() for _ in range(self.read_len(additional))]
        if major == 5:
            result = {}
            for _ in range(self.read_len(additional)):
                key = self.decode()
                if not isinstance(key, str):
                    raise TypeError("O wire map key is not a text string")
                result[key] = self.decode()
            return result
        if major == 7:
            if additional == 20:
                return False
            if additional == 21:
                return True
            if additional == 22:
                return None
            if additional == 26:
                return struct.unpack(">f", self.read(4))[0]
            if additional == 27:
                return struct.unpack(">d", self.read(8))[0]
            raise ValueError(f"unsupported CBOR simple value {additional}")

        raise ValueError(f"unsupported CBOR major type {major}")


def cbor_decode(payload):
    decoder = _CborDecoder(payload)
    value = decoder.decode()
    decoder.finish()
    return value


def _stream_or_default(stream, name):
    if stream is not None:
        return stream
    return getattr(getattr(sys, name), "buffer")


def _read_exact(stream, length):
    chunks = []
    remaining = length
    while remaining:
        chunk = stream.read(remaining)
        if not chunk:
            if length == remaining:
                return None
            raise EOFError("unexpected end of O wire frame")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def read_wire_message(stream=None):
    stream = _stream_or_default(stream, "stdin")
    header = _read_exact(stream, 4)
    if header is None:
        return None
    length = int.from_bytes(header, "big")
    if length > MAX_FRAME_LEN:
        raise ValueError(f"O wire frame length {length} exceeds maximum {MAX_FRAME_LEN}")
    payload = _read_exact(stream, length)
    if payload is None:
        raise EOFError("missing O wire frame payload")
    return cbor_decode(payload)


def write_wire_message(message, stream=None):
    stream = _stream_or_default(stream, "stdout")
    payload = cbor_encode(message)
    stream.write(len(payload).to_bytes(4, "big"))
    stream.write(payload)
    stream.flush()


def send_ok(value=None):
    write_wire_message({"status": "ok", "value": value})


def send_err(message):
    write_wire_message({"status": "err", "message": message})


def command_loop(handle_exec, handle_cleanup=None, handle_ping=None):
    while True:
        try:
            cmd = read_wire_message()
            if cmd is None:
                break
            tag = cmd.get("cmd")
            if tag == "exec":
                handle_exec(cmd)
            elif tag == "cleanup":
                if handle_cleanup is not None:
                    handle_cleanup()
                else:
                    send_ok({"t": "null"})
            elif tag == "shutdown":
                send_ok({"t": "null"})
                break
            elif tag == "ping":
                if handle_ping is not None:
                    handle_ping()
                else:
                    send_ok({"t": "null"})
            else:
                send_err(f"unknown command: {tag!r}")
        except Exception:
            import traceback

            send_err(traceback.format_exc())


def trim_stdout(output):
    """Drop the command-style trailing newline without changing other text."""
    if output.endswith("\n"):
        output = output[:-1]
        if output.endswith("\r"):
            output = output[:-1]
    return output


def int_to_oval(value):
    if INT64_MIN <= value <= INT64_MAX:
        return {"t": "int", "v": value}
    return {"t": "number", "v": {"kind": "int", "v": str(value)}}


def float_to_oval(value):
    if math.isfinite(value):
        return {"t": "float", "v": value}
    return {
        "t": "number",
        "v": {
            "kind": "binary_float",
            "format": "f64",
            "bits": list(struct.pack(">d", value)),
        },
    }


def json_value_to_oval(value):
    if value is None:
        return {"t": "null"}
    if isinstance(value, bool):
        return {"t": "bool", "v": value}
    if isinstance(value, int):
        return int_to_oval(value)
    if isinstance(value, float):
        return float_to_oval(value)
    if isinstance(value, str):
        return {"t": "str", "v": value}
    if isinstance(value, list):
        return {"t": "list", "v": [json_value_to_oval(item) for item in value]}
    if isinstance(value, dict):
        if isinstance(value.get("t"), str):
            return value
        return {
            "t": "map",
            "v": {str(key): json_value_to_oval(item) for key, item in value.items()},
        }
    return {"t": "str", "v": str(value)}


def stdout_to_oval(output):
    text = trim_stdout(output)
    stripped = text.strip()

    if stripped:
        try:
            return json_value_to_oval(json.loads(stripped))
        except Exception:
            pass

        if INT_RE.match(stripped):
            return int_to_oval(int(stripped))

        if FLOAT_RE.match(stripped):
            try:
                return float_to_oval(float(stripped))
            except Exception:
                pass

    return {"t": "str", "v": text}


def stdout_result(output):
    return stdout_to_oval(output)
