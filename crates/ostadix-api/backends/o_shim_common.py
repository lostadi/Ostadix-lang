#!/usr/bin/env python3
"""Shared helpers for backend shims.

Backend stdout is an O boundary. A script that prints a scalar or JSON value
should hand that value back to O as data, while arbitrary text remains text.
Languages can print a tagged OValue JSON envelope for exact control.
"""

import json
import hashlib
import math
import os
from pathlib import Path
import re
import stat
import struct
import sys


INT64_MIN = -(2**63)
INT64_MAX = 2**63 - 1
MAX_FRAME_LEN = 128 * 1024 * 1024
BACKEND_STATE_PROTOCOL_V1 = "ostadix.backend-state/v1"
BACKEND_STATE_CAPABILITIES_SCHEMA_V1 = "ostadix.backend-state-capabilities/v1"
BACKEND_CHECKPOINT_SCHEMA_V1 = "ostadix.backend-checkpoint/v1"
BACKEND_RESTORE_RECEIPT_SCHEMA_V1 = "ostadix.backend-restore-receipt/v1"
BACKEND_STATE_REASON_SCHEMA_V1 = "ostadix.backend-state-reason/v1"
BACKEND_STATE_ERROR_SCHEMA_V1 = "ostadix.backend-state-error/v1"
STATELESS_EMPTY_CODEC_V1 = "ostadix.backend-empty/v1"
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


class StatePinRequired(Exception):
    def __init__(self, path, message):
        super().__init__(message)
        self.path = path
        self.message = message


def backend_name_from_argv():
    name = Path(sys.argv[0]).name
    if name.endswith("_shim.py"):
        return name[: -len("_shim.py")]
    return name.removesuffix(".py")


def backend_runtime_binding_sha256():
    """Return the admitted executable-set identity, with a test-only fallback."""
    raw = os.environ.get("O_ADMITTED_EXECUTABLE_MANIFEST")
    if raw:
        try:
            digest = json.loads(raw).get("sha256")
        except (TypeError, ValueError):
            digest = None
        if isinstance(digest, str) and _is_sha256(digest):
            return digest.lower()
    identity = "\0".join(
        (
            "ostadix-python-shim-runtime/v1",
            sys.implementation.name,
            ".".join(str(part) for part in sys.version_info[:3]),
            os.path.realpath(sys.executable),
            os.path.realpath(sys.argv[0]),
        )
    )
    return hashlib.sha256(identity.encode("utf-8")).hexdigest()


def payload_sha256(payload):
    return hashlib.sha256(cbor_encode(payload)).hexdigest()


def checkpoint_sha256(checkpoint):
    return hashlib.sha256(cbor_encode(checkpoint)).hexdigest()


def state_capabilities(backend, tier="stateless", codec=STATELESS_EMPTY_CODEC_V1,
                       restore_supported=True):
    return {
        "schema": BACKEND_STATE_CAPABILITIES_SCHEMA_V1,
        "protocol": BACKEND_STATE_PROTOCOL_V1,
        "backend": backend,
        "tier": tier,
        "codec": codec,
        "scope": "backend-owned-state-at-settled-command-boundary",
        "restore_supported": bool(restore_supported),
    }


def make_checkpoint(backend, tier, codec, payload, external_resources=None,
                    runtime_binding_sha256=None):
    checkpoint = {
        "schema": BACKEND_CHECKPOINT_SCHEMA_V1,
        "protocol": BACKEND_STATE_PROTOCOL_V1,
        "backend": backend,
        "tier": tier,
        "codec": codec,
        "runtime_binding_sha256": (
            runtime_binding_sha256 or backend_runtime_binding_sha256()
        ),
        "payload": payload,
        "payload_sha256": payload_sha256(payload),
    }
    if external_resources:
        checkpoint["external_resources"] = list(external_resources)
    validate_checkpoint(checkpoint)
    return checkpoint


def validate_checkpoint(checkpoint):
    if not isinstance(checkpoint, dict):
        raise ValueError("backend checkpoint is not an object")
    allowed = {
        "schema", "protocol", "backend", "tier", "codec",
        "runtime_binding_sha256", "payload", "payload_sha256",
        "external_resources",
    }
    unknown = set(checkpoint) - allowed
    required = allowed - {"external_resources"}
    missing = required - set(checkpoint)
    if unknown or missing:
        raise ValueError(
            f"backend checkpoint has unknown={sorted(unknown)!r} missing={sorted(missing)!r}"
        )
    if checkpoint["schema"] != BACKEND_CHECKPOINT_SCHEMA_V1:
        raise ValueError("unsupported backend checkpoint schema")
    if checkpoint["protocol"] != BACKEND_STATE_PROTOCOL_V1:
        raise ValueError("unsupported backend state protocol")
    if checkpoint["tier"] not in {"stateless", "semantic_snapshot", "external_pinned"}:
        raise ValueError("unsupported backend state tier")
    for key in ("backend", "codec"):
        if not isinstance(checkpoint[key], str) or not checkpoint[key]:
            raise ValueError(f"backend checkpoint has invalid {key}")
    if not _is_sha256(checkpoint["runtime_binding_sha256"]):
        raise ValueError("backend checkpoint has invalid runtime binding")
    if not _is_sha256(checkpoint["payload_sha256"]):
        raise ValueError("backend checkpoint has invalid payload digest")
    if payload_sha256(checkpoint["payload"]) != checkpoint["payload_sha256"].lower():
        raise ValueError("backend checkpoint payload digest mismatch")
    resources = checkpoint.get("external_resources", [])
    if not isinstance(resources, list):
        raise ValueError("backend checkpoint external resources are not a list")
    for resource in resources:
        if not isinstance(resource, dict) or not all(
            isinstance(resource.get(field), str) and resource[field]
            for field in ("kind", "identity", "recovery")
        ):
            raise ValueError("backend checkpoint contains an incomplete external resource")
    if checkpoint["tier"] == "external_pinned" and not resources:
        raise ValueError("external-pinned checkpoint omitted its resource binding")
    if checkpoint["tier"] != "external_pinned" and resources:
        raise ValueError("portable backend checkpoint contains external resource bindings")
    return checkpoint


def _is_sha256(value):
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdefABCDEF" for character in value)
    )


def ensure_checkpoint_bound(checkpoint, max_bytes):
    if not isinstance(max_bytes, int) or isinstance(max_bytes, bool) or max_bytes <= 0:
        raise ValueError("checkpoint byte limit must be a positive integer")
    encoded = len(cbor_encode(checkpoint))
    if encoded > max_bytes:
        raise ValueError(
            f"checkpoint length {encoded} exceeds requested maximum {max_bytes}"
        )


def empty_checkpoint(backend):
    return make_checkpoint(
        backend,
        "stateless",
        STATELESS_EMPTY_CODEC_V1,
        {"kind": "empty"},
    )


def restore_empty_checkpoint(backend, checkpoint):
    validate_checkpoint(checkpoint)
    if (
        checkpoint["backend"] != backend
        or checkpoint["tier"] != "stateless"
        or checkpoint["codec"] != STATELESS_EMPTY_CODEC_V1
        or checkpoint["runtime_binding_sha256"] != backend_runtime_binding_sha256()
        or checkpoint["payload"] != {"kind": "empty"}
        or checkpoint.get("external_resources", [])
    ):
        raise ValueError(f"stateless checkpoint is incompatible with backend {backend!r}")


def restore_receipt(backend, checkpoint):
    return {
        "schema": BACKEND_RESTORE_RECEIPT_SCHEMA_V1,
        "protocol": BACKEND_STATE_PROTOCOL_V1,
        "backend": backend,
        "checkpoint_sha256": checkpoint_sha256(checkpoint),
        "restored": True,
    }


def _state_pin_response(backend, error):
    return {
        "status": "state_pin_required_v1",
        "reason": {
            "schema": BACKEND_STATE_REASON_SCHEMA_V1,
            "backend": backend,
            "code": "state.pin-required",
            "path": error.path,
            "message": error.message,
            "recovery": "continue-pinned",
        },
    }


def _state_error_response(backend, code, error):
    return {
        "status": "state_error_v1",
        "error": {
            "schema": BACKEND_STATE_ERROR_SCHEMA_V1,
            "backend": backend,
            "code": code,
            "message": str(error),
        },
    }


def command_loop(handle_exec, handle_cleanup=None, handle_ping=None,
                 handle_state_capabilities=None, handle_checkpoint=None,
                 handle_restore=None, state_backend=None):
    state_backend = state_backend or backend_name_from_argv()
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
            elif tag == "state_capabilities_v1":
                capabilities = (
                    handle_state_capabilities()
                    if handle_state_capabilities is not None
                    else state_capabilities(state_backend)
                )
                write_wire_message({
                    "status": "state_capabilities_v1",
                    "capabilities": capabilities,
                })
            elif tag == "checkpoint_v1":
                try:
                    checkpoint = (
                        handle_checkpoint(cmd.get("max_bytes"))
                        if handle_checkpoint is not None
                        else empty_checkpoint(state_backend)
                    )
                    ensure_checkpoint_bound(checkpoint, cmd.get("max_bytes"))
                    write_wire_message({
                        "status": "checkpoint_v1",
                        "checkpoint": checkpoint,
                    })
                except StatePinRequired as exc:
                    write_wire_message(_state_pin_response(state_backend, exc))
                except Exception as exc:
                    write_wire_message(_state_error_response(
                        state_backend, "state.checkpoint-failed", exc
                    ))
            elif tag == "restore_v1":
                try:
                    checkpoint = cmd.get("checkpoint")
                    if handle_restore is not None:
                        handle_restore(checkpoint)
                    else:
                        restore_empty_checkpoint(state_backend, checkpoint)
                    write_wire_message({
                        "status": "restore_v1",
                        "receipt": restore_receipt(state_backend, checkpoint),
                    })
                except StatePinRequired as exc:
                    write_wire_message(_state_pin_response(state_backend, exc))
                except Exception as exc:
                    write_wire_message(_state_error_response(
                        state_backend, "state.restore-incompatible", exc
                    ))
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
