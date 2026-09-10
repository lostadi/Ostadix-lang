"""Bounded owner-process retention for arbitrary Python objects.

The wire value is an inert ONative descriptor, never executable pickle or
source. Only its original store can resolve it to the retained Python object.
"""

import hmac
import json
import os
import secrets
import sys
import threading


NATIVE_OBJECT_CODEC_V1 = "ostadix.python-owner-handle/v1"
DEFAULT_MAX_NATIVE_HANDLES = 4096


def _text(value):
    return {"t": "text", "v": {"utf8": value, "encoding": None}}


def _canonical_descriptor(value):
    try:
        return json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=True,
            allow_nan=False,
        ).encode("ascii")
    except (TypeError, ValueError, RecursionError) as error:
        raise ValueError("native.invalid-handle: descriptor is not canonical data") from error


class NativeObjectStore:
    """One strong reference per explicit export, released by exact handle.

    The limit counts handles rather than estimating arbitrary object memory.
    Resolved Python references retain their ordinary Python lifetimes after
    release; release revokes future descriptor resolution, not existing refs.
    """

    def __init__(self, max_handles=None):
        if max_handles is None:
            raw = os.environ.get("O_PYTHON_MAX_NATIVE_HANDLES", str(DEFAULT_MAX_NATIVE_HANDLES))
            try:
                max_handles = int(raw)
            except (TypeError, ValueError) as error:
                raise ValueError("native.invalid-limit: O_PYTHON_MAX_NATIVE_HANDLES must be positive") from error
        if type(max_handles) is not int or max_handles <= 0:
            raise ValueError("native.invalid-limit: native handle limit must be a positive integer")
        self._limit = max_handles
        self._origin = secrets.token_hex(32)
        self._pid = os.getpid()
        self._entries = {}
        self._lock = threading.RLock()

    def _require_original_process(self):
        if os.getpid() != self._pid:
            raise ValueError("native.owner-mismatch: forked processes cannot use the original owner store")

    @property
    def live_count(self):
        self._require_original_process()
        with self._lock:
            return len(self._entries)

    def export(self, value):
        self._require_original_process()
        with self._lock:
            if len(self._entries) >= self._limit:
                raise ValueError("native.capacity-exhausted: release a native handle or increase O_PYTHON_MAX_NATIVE_HANDLES")
            token = secrets.token_hex(32)
            while token in self._entries:
                token = secrets.token_hex(32)
            kind = type(value)
            # Read type's own descriptors directly. type.__getattribute__ still
            # dispatches a metaclass property and could execute user code.
            try:
                module = type.__dict__["__module__"].__get__(kind)
                name = type.__dict__["__qualname__"].__get__(kind)
            except (AttributeError, TypeError):
                module, name = "unknown", "object"
            module = module if type(module) is str else "unknown"
            name = name if type(name) is str else "object"
            descriptor = {
                "t": "native",
                "v": {
                    "lang": "python",
                    "implementation": sys.implementation.name,
                    "version": ".".join(str(part) for part in sys.version_info[:3]),
                    "type_name": f"{module}.{name}",
                    "identity": {"stable": None, "live": f"python:{self._origin}:{token}"},
                    "codec": NATIVE_OBJECT_CODEC_V1,
                    "payload": None,
                    "boundary": "effectful",
                    "safety": "live_handle",
                    "capabilities": [],
                    "metadata": {
                        "owner": _text(self._origin),
                        "lifetime": _text("owner-process-until-release"),
                        "equivalence": _text("same-python-object"),
                        "session": _text(os.environ.get("O_BACKEND_SESSION_ID", "unmanaged")),
                        "generation": _text(os.environ.get("O_BACKEND_LAUNCH_GENERATION", "unmanaged")),
                        "instance": _text(os.environ.get("O_BACKEND_NATIVE_INSTANCE_ID", "unmanaged")),
                    },
                    "rehydrate": "same_process",
                },
            }
            sealed = _canonical_descriptor(descriptor)
            self._entries[token] = (value, sealed)
            # The retained descriptor seal shares no mutable data with callers.
            return descriptor

    def _lookup(self, descriptor):
        self._require_original_process()
        if type(descriptor) is not dict or descriptor.get("t") != "native":
            raise TypeError("native.invalid-handle: expected an O native descriptor")
        capsule = descriptor.get("v")
        identity = capsule.get("identity") if type(capsule) is dict else None
        live = identity.get("live") if type(identity) is dict else None
        parts = live.split(":") if type(live) is str else []
        if len(parts) != 3 or parts[0] != "python":
            raise ValueError("native.invalid-handle: malformed live identity")
        origin, token = parts[1:]
        if any(len(part) != 64 or any(character not in "0123456789abcdef" for character in part)
               for part in (origin, token)):
            raise ValueError("native.invalid-handle: owner and token must be canonical 256-bit identities")
        if not hmac.compare_digest(origin, self._origin):
            raise ValueError("native.owner-mismatch: handle belongs to another or expired owner process")
        entry = self._entries.get(token)
        if entry is None:
            raise ValueError("native.handle-expired: handle was released, cleaned up, or is unknown")
        if not hmac.compare_digest(_canonical_descriptor(descriptor), entry[1]):
            raise ValueError("native.altered-handle: descriptor differs from its exported identity")
        return token, entry[0]

    def resolve(self, descriptor):
        self._require_original_process()
        with self._lock:
            return self._lookup(descriptor)[1]

    def release(self, descriptor):
        self._require_original_process()
        with self._lock:
            token, value = self._lookup(descriptor)
            del self._entries[token]
        # Keep the value alive until the lock is released so finalizers do not
        # execute inside this store's critical section.
        del value

    def clear(self):
        self._require_original_process()
        with self._lock:
            previous = self._entries
            self._entries = {}
        previous.clear()
