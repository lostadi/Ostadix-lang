#!/usr/bin/env python3
import sys
import json
import io
import ast
import contextlib
import base64
import decimal
import fractions
import math
import os
import struct
import traceback
import textwrap
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from o_shim_common import (
    StatePinRequired,
    backend_runtime_binding_sha256,
    command_loop,
    make_checkpoint,
    state_capabilities,
    validate_checkpoint,
    read_wire_message,
    write_wire_message,
)
from o_native_objects import NativeObjectStore

_native_objects = NativeObjectStore()
_active_native_operation = False

# Save a reference to the real process stdout (fd 1) before anything can
# redirect it. O.eval() must write eval_request directly over the IPC pipe
# even when the shim's handle_exec has temporarily redirected sys.stdout to
# a StringIO capture buffer for print() capture.
_real_stdout = sys.stdout
_current_o_scope = {}
_current_o_scope_wire = {}
_INT64_MIN = -(2 ** 63)
_INT64_MAX = 2 ** 63 - 1
PYTHON_GRAPH_CODEC_V1 = "ostadix.python-graph/v1"
_DECIMAL_CHUNK_DIGITS = 256
_DECIMAL_CHUNK_BASE = 10 ** _DECIMAL_CHUNK_DIGITS


def _decimal_text_to_int(text):
    """Parse an arbitrary decimal integer without CPython's digit ceiling."""
    text = str(text)
    negative = text.startswith("-")
    digits = text[1:] if negative else text
    if not digits or not digits.isascii() or not digits.isdigit():
        raise ValueError("invalid decimal integer")
    value = 0
    for offset in range(0, len(digits), _DECIMAL_CHUNK_DIGITS):
        chunk = digits[offset : offset + _DECIMAL_CHUNK_DIGITS]
        value = value * (10 ** len(chunk)) + int(chunk)
    return -value if negative else value


def _int_to_decimal_text(value):
    """Format an arbitrary integer without CPython's digit ceiling."""
    if value == 0:
        return "0"
    negative = value < 0
    value = abs(value)
    chunks = []
    while value:
        value, remainder = divmod(value, _DECIMAL_CHUNK_BASE)
        chunks.append(remainder)
    text = str(chunks.pop())
    text += "".join(f"{chunk:0{_DECIMAL_CHUNK_DIGITS}d}" for chunk in reversed(chunks))
    return f"-{text}" if negative else text

def dump_generated_python(source):
    try:
        override = os.environ.get("O_PYTHON_DUMP_FILE")
        path = (
            Path(override)
            if override
            else Path(os.environ.get("TMPDIR", "/tmp")) / f"O-python-failing-{os.getpid()}.py"
        )
        path.write_text(source, encoding="utf-8")
        return str(path)
    except Exception as exc:
        return f"<failed to write generated Python source: {exc}>"

class OHtml(str):
    """Typed trusted HTML fragment passed through O-lang."""
    def __new__(cls, value):
        return str.__new__(cls, value)

class OStorePath(str):
    """Typed Nix store path passed through O-lang."""
    def __new__(cls, value):
        return str.__new__(cls, value)

class OExprValue:
    """A quoted but unevaluated O expression (OValue::Expr on the Rust side).

    Created by ``quote^(...)_quote`` blocks and by ``O.quote(src)``.
    Evaluated by passing it to ``O.eval(q)``.
    """
    def __init__(self, src: str):
        self.src = src

    def __repr__(self):
        return f"OExprValue({self.src!r})"

    def __str__(self):
        return self.src


class OOpaqueValue:
    """A lossless Python handle for an OValue without a native Python form."""

    def __init__(self, wire_value):
        if not isinstance(wire_value, dict) or "t" not in wire_value:
            raise TypeError("OOpaqueValue requires a tagged OValue object")
        self.wire_value = dict(wire_value)

    def __repr__(self):
        return f"OOpaqueValue({self.wire_value.get('t')!r})"

    @classmethod
    def from_wire_json(cls, encoded):
        return cls(json.loads(encoded))


class OScopeValue:
    """A detached snapshot of O-level lexical bindings."""

    def __init__(self, bindings, wire_bindings=None):
        if not isinstance(bindings, dict):
            raise TypeError("OScopeValue bindings must be a dict")
        self.bindings = dict(bindings)
        self.wire_bindings = (
            dict(wire_bindings) if wire_bindings is not None else None
        )

    def __repr__(self):
        return f"OScopeValue({self.bindings!r})"

    @classmethod
    def from_wire_json(cls, encoded):
        """Rebuild a scope literal without erasing opaque nested OValues."""
        value = oval_to_py(json.loads(encoded))
        if not isinstance(value, cls):
            raise TypeError("OScopeValue wire literal did not contain a scope")
        return value


class _OMod:
    """The ``O`` namespace injected into every Python block.

    Provides ``O.eval(q)`` for evaluating a quoted expression and
    ``O.quote(src)`` for constructing one from a source string.
    """

    @staticmethod
    def native(value):
        """Retain any Python object and return its owner-process descriptor.

        Use a persistent python[N] actor to resolve the handle in later blocks.
        Fresh actors expire when their block finishes. Explicit native_call,
        native_get and native_set route operations to the retained owner;
        carrying the descriptor never reconstructs the Python object.
        """
        if _active_morphism_contract is not None:
            raise TypeError("morphism.unsupported-native: owner handles are outside the plain-data contract")
        return OOpaqueValue(_native_objects.export(value))

    @staticmethod
    def resolve_native(handle):
        """Return the exact retained object in its original owning process."""
        if not isinstance(handle, OOpaqueValue):
            raise TypeError("native.invalid-handle: expected an O.native handle")
        return _native_objects.resolve(handle.wire_value)

    @staticmethod
    def release_native(handle):
        """Invalidate this export and all copies of its descriptor."""
        if not isinstance(handle, OOpaqueValue):
            raise TypeError("native.invalid-handle: expected an O.native handle")
        _native_objects.release(handle.wire_value)

    @staticmethod
    def native_call(handle, *args):
        return _OMod.eval("native_call($handle, $args)",
                         _OMod.scope({"handle": handle, "args": list(args)}))

    @staticmethod
    def native_get(handle, name):
        return _OMod.eval("native_get($handle, $name)",
                         _OMod.scope({"handle": handle, "name": name}))

    @staticmethod
    def native_set(handle, name, value):
        return _OMod.eval("native_set($handle, $name, $value)",
                         _OMod.scope({"handle": handle, "name": name, "value": value}))

    @staticmethod
    def native_release(handle):
        return _OMod.eval("native_release($handle)", _OMod.scope({"handle": handle}))

    @staticmethod
    def eval(q, scope_snapshot=None):
        """Evaluate a quoted expression and return its result.

        Sends an ``eval_request`` back to the Rust runtime, which evaluates
        the O source fragment and replies with an ``eval_result`` command.
        The function then returns the result as a Python value.

        With one argument, the O fragment sees the lexical snapshot visible at
        this backend call site. With ``O.eval(q, scope_snapshot)``, it instead
        uses the supplied ``OScopeValue``. Bindings created by the fragment
        remain local to that evaluation in both forms.

        ``O.eval(q)`` cannot be used if ``q`` contains a
        reference to the same persistent env that is currently executing
        (e.g. ``python[0]^(...)_python[0]`` inside another
        ``python[0]^(...)_python[0]`` block), as this would deadlock the
        subprocess protocol. Use ephemeral or different-env blocks.
        """
        if isinstance(q, OExprValue):
            src = q.src
        elif isinstance(q, str):
            src = q
        else:
            raise TypeError(
                f"O.eval expects an OExprValue (from quote^...) or a str, "
                f"got {type(q).__name__!r}"
            )
        # Write directly to the real process stdout (fd 1) to bypass any
        # contextlib.redirect_stdout() that the handle_exec caller installs
        # for capturing print() output.  The IPC protocol must go over the
        # real pipe — not the StringIO capture buffer.
        msg = {"status": "eval_request", "src": src}
        if _active_native_operation and scope_snapshot is None:
            msg["scope"] = {"t": "scope", "bindings": dict(_current_o_scope_wire)}
        if scope_snapshot is not None:
            if not isinstance(scope_snapshot, OScopeValue):
                raise TypeError(
                    "O.eval explicit scope must be an OScopeValue from "
                    f"scope() or O.scope(), got {type(scope_snapshot).__name__!r}"
                )
            if _active_morphism_contract is not None:
                scope_seen = set()
                msg["scope"] = {"t": "scope", "bindings": {
                    name: _lossless_native_witness(value, f"$callback_scope.{name}", scope_seen)
                    for name, value in scope_snapshot.bindings.items()
                }}
            else:
                msg["scope"] = py_to_oval(scope_snapshot)
        write_wire_message(msg, _real_stdout.buffer)
        # Block until the runtime replies with eval_result.
        resp = read_wire_message(sys.stdin.buffer)
        while resp is not None and resp.get("cmd") == "native_operation_v1":
            handle_native_operation(resp)
            resp = read_wire_message(sys.stdin.buffer)
        if resp is None:
            raise RuntimeError("O.eval: runtime closed stdin before sending eval_result")
        if resp.get("cmd") != "eval_result":
            raise RuntimeError(
                f"O.eval: expected eval_result command, got {resp.get('cmd')!r}"
            )
        value = resp.get("value", {"t": "null"})
        if value.get("t") == "error" and (_active_native_operation or src.startswith("native_")):
            raise RuntimeError(value.get("msg", "native callback failed"))
        if _active_morphism_contract is not None:
            return _lossless_input(value, "$callback")
        return oval_to_py(value)

    @staticmethod
    def quote(src: str) -> OExprValue:
        """Construct a quoted O expression from a source string.

        The source is stored verbatim and not evaluated here. Pass the
        result to ``O.eval(q)`` to evaluate it.

        Note: if the source string contains opener syntax (e.g.
        ``python^(``) that shouldn't be parsed by the O parser, you must
        have escaped them with a backslash (``\\python^(``) in the
        *outer* O source. The backslash is consumed by the O parser and
        the literal text ``python^(`` reaches the Python code.
        """
        if not isinstance(src, str):
            raise TypeError(f"O.quote expects a str, got {type(src).__name__!r}")
        return OExprValue(src)

    @staticmethod
    def scope(bindings=None) -> OScopeValue:
        """Capture the current O lexical bindings or build an explicit scope."""
        if bindings is None:
            return OScopeValue(_current_o_scope, _current_o_scope_wire)
        if not isinstance(bindings, dict):
            raise TypeError(f"O.scope expects a dict, got {type(bindings).__name__!r}")
        return OScopeValue(bindings)


def oval_to_py(v):
    t = v.get("t")

    if t == "null":
        return None
    if t == "bool":
        return bool(v.get("v"))
    if t == "int":
        return int(v.get("v"))
    if t == "float":
        return float(v.get("v"))
    if t == "number":
        return oval_number_to_py(v.get("v", {}))
    if t == "str":
        return str(v.get("v"))
    if t == "text":
        return str(v.get("v", {}).get("utf8", ""))
    if t == "bytes":
        return bytes(v.get("v", {}).get("bytes", []))
    if t == "char":
        return str(v.get("scalar", ""))
    if t == "html":
        return OHtml(v.get("v", ""))
    if t == "store_path":
        return OStorePath(v.get("path", ""))
    if t == "list":
        return [oval_to_py(x) for x in v.get("v", [])]
    if t == "map":
        return {k: oval_to_py(x) for k, x in v.get("v", {}).items()}
    if t == "seq":
        items = [oval_to_py(x) for x in v.get("items", [])]
        return tuple(items) if v.get("kind") == "tuple" else items
    if t == "object":
        return {k: oval_to_py(x) for k, x in v.get("fields", {}).items()}
    if t == "entries_map":
        return [(oval_to_py(k), oval_to_py(val)) for k, val in v.get("entries", [])]
    if t == "set":
        items = [oval_to_py(x) for x in v.get("items", [])]
        try:
            return set(items)
        except TypeError:
            return items
    if t == "symbol":
        sym = v.get("v", {})
        ns = sym.get("namespace")
        name = sym.get("name", "")
        return f"{ns}/{name}" if ns else name
    if t == "keyword":
        kw = v.get("v", {})
        ns = kw.get("namespace")
        name = kw.get("name", "")
        return f":{ns}/{name}" if ns else f":{name}"
    if t == "scope":
        wire_bindings = v.get("bindings", {})
        return OScopeValue(
            {k: _scope_binding_to_py(x) for k, x in wire_bindings.items()},
            wire_bindings,
        )
    if t == "blob":
        try:
            return base64.b64decode(v.get("v", ""), validate=True)
        except (TypeError, ValueError):
            return OOpaqueValue(v)
    if t == "expr":
        return OExprValue(v.get("src", ""))

    return OOpaqueValue(v)


def _scope_binding_to_py(value):
    """Keep malformed nested public values inert while retaining scope wire."""
    try:
        return oval_to_py(value)
    except Exception:
        return OOpaqueValue(value)


def oval_number_to_py(n):
    kind = n.get("kind")
    if kind == "int":
        try:
            return _decimal_text_to_int(n.get("v", "0"))
        except ValueError:
            return OOpaqueValue({"t": "number", "v": n})
    if kind == "rational":
        try:
            numerator = _decimal_text_to_int(n.get("num", "0"))
            denominator = _decimal_text_to_int(n.get("den", "1"))
            if denominator == 0:
                return OOpaqueValue({"t": "number", "v": n})
            return fractions.Fraction(numerator, denominator)
        except ValueError:
            return OOpaqueValue({"t": "number", "v": n})
    if kind == "decimal":
        special = n.get("special")
        try:
            coeff = _decimal_text_to_int(n.get("coeff", "0"))
            exponent = int(n.get("exp10", 0))
        except (TypeError, ValueError):
            return OOpaqueValue({"t": "number", "v": n})
        if special is not None and (coeff != 0 or exponent != 0):
            return OOpaqueValue({"t": "number", "v": n})
        if special == "nan":
            return decimal.Decimal("NaN")
        if special == "pos_inf":
            return decimal.Decimal("Infinity")
        if special == "neg_inf":
            return decimal.Decimal("-Infinity")
        if special == "pos_zero":
            return decimal.Decimal("0")
        if special == "neg_zero":
            return decimal.Decimal("-0")
        if special is not None:
            return OOpaqueValue({"t": "number", "v": n})
        literal = f"{n.get('coeff', '0')}e{exponent}"
        try:
            return decimal.Decimal(literal)
        except (decimal.InvalidOperation, ValueError):
            return OOpaqueValue({"t": "number", "v": n})
    if kind == "binary_float":
        try:
            bits = bytes(n.get("bits", []))
        except (TypeError, ValueError):
            return OOpaqueValue({"t": "number", "v": n})
        if n.get("format") == "f32" and len(bits) == 4:
            return struct.unpack(">f", bits)[0]
        if n.get("format") == "f64" and len(bits) == 8:
            return struct.unpack(">d", bits)[0]
        return OOpaqueValue({"t": "number", "v": n})
    if kind == "complex":
        real = oval_number_to_py(n.get("re", {"kind": "int", "v": "0"}))
        imaginary = oval_number_to_py(n.get("im", {"kind": "int", "v": "0"}))
        if isinstance(real, OOpaqueValue) or isinstance(imaginary, OOpaqueValue):
            return OOpaqueValue({"t": "number", "v": n})
        try:
            return complex(real, imaginary)
        except (OverflowError, TypeError, ValueError):
            return OOpaqueValue({"t": "number", "v": n})
    return OOpaqueValue({"t": "number", "v": n})


def py_number_to_oval_payload(x):
    if isinstance(x, int):
        return {"kind": "int", "v": _int_to_decimal_text(x)}

    if isinstance(x, fractions.Fraction):
        return {
            "kind": "rational",
            "num": _int_to_decimal_text(x.numerator),
            "den": _int_to_decimal_text(x.denominator),
        }

    if isinstance(x, decimal.Decimal):
        if x.is_nan():
            return {"kind": "decimal", "coeff": "0", "exp10": 0, "special": "nan"}
        if x == decimal.Decimal("Infinity"):
            return {"kind": "decimal", "coeff": "0", "exp10": 0, "special": "pos_inf"}
        if x == decimal.Decimal("-Infinity"):
            return {"kind": "decimal", "coeff": "0", "exp10": 0, "special": "neg_inf"}
        if x.is_zero():
            return {
                "kind": "decimal",
                "coeff": "0",
                "exp10": 0,
                "special": "neg_zero" if x.is_signed() else "pos_zero",
            }
        sign, digits, exponent = x.as_tuple()
        coeff = "".join(str(digit) for digit in digits).lstrip("0") or "0"
        if sign and coeff != "0":
            coeff = f"-{coeff}"
        return {"kind": "decimal", "coeff": coeff, "exp10": int(exponent), "special": None}

    if isinstance(x, float):
        return {
            "kind": "binary_float",
            "format": "f64",
            "bits": list(struct.pack(">d", x)),
        }

    if isinstance(x, complex):
        return {
            "kind": "complex",
            "re": py_number_to_oval_payload(float(x.real)),
            "im": py_number_to_oval_payload(float(x.imag)),
        }

    raise TypeError(f"not a supported numeric value: {type(x).__name__}")


def py_to_oval(x):
    if x is None:
        return {"t": "null"}

    if isinstance(x, bool):
        return {"t": "bool", "v": x}

    if isinstance(x, int):
        if _INT64_MIN <= x <= _INT64_MAX:
            return {"t": "int", "v": x}
        return {"t": "number", "v": py_number_to_oval_payload(x)}

    if isinstance(x, (fractions.Fraction, decimal.Decimal, complex)):
        return {"t": "number", "v": py_number_to_oval_payload(x)}

    if isinstance(x, float):
        if math.isfinite(x):
            return {"t": "float", "v": x}
        return {"t": "number", "v": py_number_to_oval_payload(x)}

    if isinstance(x, OHtml):
        return {"t": "html", "v": str(x)}

    if isinstance(x, OStorePath):
        return {"t": "store_path", "path": str(x)}

    if isinstance(x, OExprValue):
        return {"t": "expr", "src": x.src}

    if isinstance(x, OOpaqueValue):
        return dict(x.wire_value)

    if isinstance(x, OScopeValue):
        return {
            "t": "scope",
            "bindings": (
                dict(x.wire_bindings)
                if x.wire_bindings is not None
                else {k: py_to_oval(v) for k, v in x.bindings.items()}
            ),
        }

    if isinstance(x, str):
        return {"t": "str", "v": x}

    if isinstance(x, (bytes, bytearray, memoryview)):
        return {
            "t": "bytes",
            "v": {
                "bytes": list(bytes(x)),
                "media_type": "application/octet-stream",
            },
        }

    # matplotlib.figure.Figure -> PNG blob (for computed plots etc in HTML)
    try:
        import matplotlib.figure
        if isinstance(x, matplotlib.figure.Figure):
            buf = io.BytesIO()
            x.savefig(buf, format="png", bbox_inches="tight", dpi=120)
            return {
                "t": "blob",
                "v": base64.b64encode(buf.getvalue()).decode("ascii"),
                "mime": "image/png",
            }
    except Exception:
        pass

    # PIL.Image -> PNG blob
    try:
        from PIL import Image as _PILImage
        if isinstance(x, _PILImage.Image):
            buf = io.BytesIO()
            x.save(buf, format="PNG")
            return {
                "t": "blob",
                "v": base64.b64encode(buf.getvalue()).decode("ascii"),
                "mime": "image/png",
            }
    except Exception:
        pass

    if isinstance(x, tuple):
        return {"t": "seq", "kind": "tuple", "items": [py_to_oval(i) for i in x]}

    if isinstance(x, list):
        return {"t": "list", "v": [py_to_oval(i) for i in x]}

    if isinstance(x, (set, frozenset)):
        return {
            "t": "set",
            "kind": "unordered",
            "items": [py_to_oval(i) for i in x],
        }

    if isinstance(x, dict):
        if all(isinstance(k, str) for k in x):
            return {"t": "map", "v": {k: py_to_oval(v) for k, v in x.items()}}
        return {
            "t": "entries_map",
            "entries": [[py_to_oval(k), py_to_oval(v)] for k, v in x.items()],
        }

    # Never turn an unknown Python object into apparently lossless O text.
    # Explicit owner-process retention is available through O.native(value).
    # Never silently allocate a handle or erase unsupported object semantics.
    raise TypeError(
        "unsupported Python value for OValue projection: "
        f"{type(x).__module__}.{type(x).__qualname__}"
    )

def send_ok(value=None):
    write_wire_message({"status": "ok", "value": py_to_oval(value)}, _real_stdout.buffer)

def send_err(message):
    write_wire_message({"status": "err", "message": message}, _real_stdout.buffer)

O = _OMod()
env = {
    "OHtml": OHtml,
    "OStorePath": OStorePath,
    "OExprValue": OExprValue,
    "OOpaqueValue": OOpaqueValue,
    "OScopeValue": OScopeValue,
    "O": O,
}
_BASE_ENV = dict(env)


def _ambient_fingerprint():
    context = decimal.getcontext()
    payload = {
        "cwd": os.getcwd(),
        "environment": [
            [key, value]
            for key, value in sorted(os.environ.items())
            if key not in {"O_BACKEND_SESSION_ID", "O_BACKEND_NATIVE_INSTANCE_ID", "O_LIFECYCLE_TRACE"}
        ],
        "sys_path": list(sys.path),
        "decimal_context": {
            "prec": context.prec,
            "rounding": context.rounding,
            "emin": context.Emin,
            "emax": context.Emax,
            "capitals": context.capitals,
            "clamp": context.clamp,
            "flags": sorted(signal.__name__ for signal, active in context.flags.items() if active),
            "traps": sorted(signal.__name__ for signal, active in context.traps.items() if active),
        },
    }
    import hashlib
    from o_shim_common import cbor_encode
    return hashlib.sha256(cbor_encode(payload)).hexdigest()


_BASE_AMBIENT_SHA256 = _ambient_fingerprint()


def _user_globals_are_clean():
    for name, value in env.items():
        if name == "__builtins__":
            continue
        if name not in _BASE_ENV or value is not _BASE_ENV[name]:
            return False
    return all(name in env and env[name] is value for name, value in _BASE_ENV.items())


class _PythonGraphEncoder:
    def __init__(self):
        self.nodes = []
        self.memo = {}

    def encode(self, value, path):
        if value is None:
            return {"kind": "none"}
        if type(value) is bool:
            return {"kind": "bool", "value": value}
        if type(value) is int:
            return {"kind": "int", "value": _int_to_decimal_text(value)}
        if type(value) is float:
            return {
                "kind": "float64",
                "bits": list(struct.pack(">d", value)),
            }
        if type(value) is OHtml:
            return {"kind": "o_html", "value": str(value)}
        if type(value) is OStorePath:
            return {"kind": "o_store_path", "value": str(value)}
        if type(value) is str:
            return {"kind": "str", "value": value}
        if type(value) is bytes:
            return {
                "kind": "bytes",
                "value_b64": base64.b64encode(value).decode("ascii"),
            }
        if type(value) is decimal.Decimal:
            return {"kind": "decimal", "value": str(value)}
        if type(value) is fractions.Fraction:
            return {
                "kind": "fraction",
                "numerator": _int_to_decimal_text(value.numerator),
                "denominator": _int_to_decimal_text(value.denominator),
            }
        if type(value) is complex:
            return {
                "kind": "complex",
                "real_bits": list(struct.pack(">d", value.real)),
                "imag_bits": list(struct.pack(">d", value.imag)),
            }
        if type(value) is OExprValue:
            return {"kind": "o_expr", "src": value.src}
        if type(value) is OOpaqueValue:
            return {"kind": "o_opaque", "wire_value": value.wire_value}
        if type(value) in (list, dict, tuple, bytearray, OScopeValue):
            return self._encode_node(value, path)
        raise StatePinRequired(
            path,
            "unsupported Python object "
            f"{type(value).__module__}.{type(value).__qualname__}; "
            "continue this session on its current actor",
        )

    def _encode_node(self, value, path):
        identity = id(value)
        if identity in self.memo:
            return {"kind": "ref", "node": self.memo[identity]}
        node_id = len(self.nodes)
        self.memo[identity] = node_id
        self.nodes.append(None)
        if type(value) is list:
            node = {
                "kind": "list",
                "items": [
                    self.encode(item, f"{path}[{index}]")
                    for index, item in enumerate(value)
                ],
            }
        elif type(value) is tuple:
            node = {
                "kind": "tuple",
                "items": [
                    self.encode(item, f"{path}[{index}]")
                    for index, item in enumerate(value)
                ],
            }
        elif type(value) is dict:
            entries = []
            for index, (key, item) in enumerate(value.items()):
                entries.append([
                    self.encode(key, f"{path}.key[{index}]"),
                    self.encode(item, f"{path}.value[{index}]"),
                ])
            node = {"kind": "dict", "entries": entries}
        elif type(value) is bytearray:
            node = {
                "kind": "bytearray",
                "value_b64": base64.b64encode(bytes(value)).decode("ascii"),
            }
        else:
            node = {
                "kind": "o_scope",
                "bindings": self.encode(value.bindings, f"{path}.bindings"),
                "wire_bindings": (
                    self.encode(value.wire_bindings, f"{path}.wire_bindings")
                    if value.wire_bindings is not None
                    else {"kind": "none"}
                ),
            }
        self.nodes[node_id] = node
        return {"kind": "ref", "node": node_id}


class _PythonGraphDecoder:
    def __init__(self, nodes):
        if not isinstance(nodes, list):
            raise ValueError("Python graph nodes are not a list")
        self.nodes = nodes
        self.objects = [None] * len(nodes)
        self.building_tuples = set()
        for node_id, node in enumerate(nodes):
            if not isinstance(node, dict):
                raise ValueError(f"Python graph node {node_id} is not an object")
            kind = node.get("kind")
            if kind == "list":
                self.objects[node_id] = []
            elif kind == "dict":
                self.objects[node_id] = {}
            elif kind == "bytearray":
                self.objects[node_id] = bytearray()
            elif kind == "o_scope":
                self.objects[node_id] = OScopeValue.__new__(OScopeValue)
            elif kind != "tuple":
                raise ValueError(f"unsupported Python graph node kind {kind!r}")

    def decode(self, encoded):
        if not isinstance(encoded, dict):
            raise ValueError("Python graph value is not an object")
        kind = encoded.get("kind")
        if kind == "none":
            return None
        if kind == "bool":
            return bool(encoded["value"])
        if kind == "int":
            return _decimal_text_to_int(encoded["value"])
        if kind == "float64":
            return struct.unpack(">d", bytes(encoded["bits"]))[0]
        if kind == "str":
            return str(encoded["value"])
        if kind == "bytes":
            return base64.b64decode(encoded["value_b64"], validate=True)
        if kind == "decimal":
            return decimal.Decimal(encoded["value"])
        if kind == "fraction":
            return fractions.Fraction(
                _decimal_text_to_int(encoded["numerator"]),
                _decimal_text_to_int(encoded["denominator"]),
            )
        if kind == "complex":
            return complex(
                struct.unpack(">d", bytes(encoded["real_bits"]))[0],
                struct.unpack(">d", bytes(encoded["imag_bits"]))[0],
            )
        if kind == "o_html":
            return OHtml(encoded["value"])
        if kind == "o_store_path":
            return OStorePath(encoded["value"])
        if kind == "o_expr":
            return OExprValue(encoded["src"])
        if kind == "o_opaque":
            return OOpaqueValue(encoded["wire_value"])
        if kind == "ref":
            return self._materialize_node(encoded.get("node"))
        raise ValueError(f"unsupported Python graph scalar kind {kind!r}")

    def _materialize_node(self, node_id):
        if type(node_id) is not int or not 0 <= node_id < len(self.nodes):
            raise ValueError(f"Python graph reference {node_id!r} is out of range")
        node = self.nodes[node_id]
        kind = node["kind"]
        if kind == "tuple":
            if self.objects[node_id] is not None:
                return self.objects[node_id]
            if node_id in self.building_tuples:
                raise ValueError("Python graph contains an impossible direct tuple cycle")
            self.building_tuples.add(node_id)
            try:
                value = tuple(self.decode(item) for item in node.get("items", []))
                self.objects[node_id] = value
            finally:
                self.building_tuples.remove(node_id)
            return value
        return self.objects[node_id]

    def finish(self):
        for node_id, node in enumerate(self.nodes):
            if node["kind"] == "tuple":
                self._materialize_node(node_id)
        for node_id, node in enumerate(self.nodes):
            target = self.objects[node_id]
            kind = node["kind"]
            if kind == "list":
                target.extend(self.decode(item) for item in node.get("items", []))
            elif kind == "dict":
                for entry in node.get("entries", []):
                    if not isinstance(entry, list) or len(entry) != 2:
                        raise ValueError("Python graph dictionary entry is malformed")
                    target[self.decode(entry[0])] = self.decode(entry[1])
            elif kind == "bytearray":
                target.extend(base64.b64decode(node["value_b64"], validate=True))
            elif kind == "o_scope":
                bindings = self.decode(node["bindings"])
                wire_bindings = self.decode(node["wire_bindings"])
                if not isinstance(bindings, dict):
                    raise ValueError("Python scope bindings are not a dictionary")
                if wire_bindings is not None and not isinstance(wire_bindings, dict):
                    raise ValueError("Python scope wire bindings are not a dictionary")
                target.bindings = bindings
                target.wire_bindings = wire_bindings


def _encode_python_globals():
    encoder = _PythonGraphEncoder()
    globals_payload = []
    for name in sorted(env):
        if name == "__builtins__":
            continue
        value = env[name]
        if name in _BASE_ENV and value is _BASE_ENV[name]:
            continue
        globals_payload.append([name, encoder.encode(value, f"$globals[{name!r}]")])
    deleted_baseline = sorted(name for name in _BASE_ENV if name not in env)
    return {
        "profile": "constrained-python-graph",
        "ambient_sha256": _BASE_AMBIENT_SHA256,
        "globals": globals_payload,
        "deleted_baseline": deleted_baseline,
        "nodes": encoder.nodes,
    }


def _decode_python_globals(payload):
    if not isinstance(payload, dict) or payload.get("profile") != "constrained-python-graph":
        raise ValueError("Python checkpoint uses an unsupported graph profile")
    if payload.get("ambient_sha256") != _BASE_AMBIENT_SHA256:
        raise ValueError("Python checkpoint ambient process binding does not match")
    decoder = _PythonGraphDecoder(payload.get("nodes"))
    globals_payload = payload.get("globals")
    if not isinstance(globals_payload, list):
        raise ValueError("Python checkpoint globals are not a list")
    restored = {}
    for entry in globals_payload:
        if not isinstance(entry, list) or len(entry) != 2 or not isinstance(entry[0], str):
            raise ValueError("Python checkpoint global entry is malformed")
        if entry[0] in restored or entry[0] == "__builtins__":
            raise ValueError(f"Python checkpoint repeats or reserves global {entry[0]!r}")
        restored[entry[0]] = entry[1]
    decoder.finish()
    restored = {name: decoder.decode(value) for name, value in restored.items()}
    deleted = payload.get("deleted_baseline", [])
    if not isinstance(deleted, list) or any(name not in _BASE_ENV for name in deleted):
        raise ValueError("Python checkpoint deleted-baseline set is invalid")
    return restored, deleted


def handle_state_capabilities():
    return state_capabilities(
        "python", "semantic_snapshot", PYTHON_GRAPH_CODEC_V1, True
    )


def handle_checkpoint(max_bytes):
    if _native_objects.live_count:
        raise StatePinRequired(
            "$native_handles",
            "exported Python native handles retain owner-process objects; release every handle before checkpoint or migration",
        )
    if _ambient_fingerprint() != _BASE_AMBIENT_SHA256:
        raise StatePinRequired(
            "$process.ambient",
            "the Python session changed cwd, environment, sys.path, or decimal context",
        )
    return make_checkpoint(
        "python",
        "semantic_snapshot",
        PYTHON_GRAPH_CODEC_V1,
        _encode_python_globals(),
    )


def handle_restore(checkpoint):
    global _current_o_scope, _current_o_scope_wire
    validate_checkpoint(checkpoint)
    if not _user_globals_are_clean():
        raise ValueError("state.restore-conflict: Python actor already owns user state")
    if (
        checkpoint["backend"] != "python"
        or checkpoint["tier"] != "semantic_snapshot"
        or checkpoint["codec"] != PYTHON_GRAPH_CODEC_V1
        or checkpoint["runtime_binding_sha256"] != backend_runtime_binding_sha256()
        or checkpoint.get("external_resources", [])
    ):
        raise ValueError("Python checkpoint is incompatible with this runtime")
    restored, deleted = _decode_python_globals(checkpoint["payload"])
    replacement = dict(_BASE_ENV)
    for name in deleted:
        replacement.pop(name, None)
    replacement.update(restored)
    env.clear()
    env.update(replacement)
    _current_o_scope = {}
    _current_o_scope_wire = {}

_active_morphism_contract = None
_PLAIN_DATA_CONTRACT_V1 = "python-plain-data-lossless"


def _lossless_native_witness(value, path="$", seen=None, depth=0):
    """Exact plain-data observation before projection; never invokes user codecs.

    Identity and arbitrary Python interactions are outside this contract. Shared
    or cyclic containers are rejected, rather than silently copying their graph.
    """
    if depth > 64:
        raise TypeError(f"morphism.depth-limit: {path}")
    kind = type(value)
    if value is None:
        return {"t": "null"}
    if kind is bool:
        return {"t": "bool", "v": value}
    if kind is int:
        return {"t": "number", "v": {"kind": "int", "v": _int_to_decimal_text(value)}}
    if kind is float:
        if not math.isfinite(value):
            raise TypeError(f"morphism.nonfinite-float: {path}")
        return {"t": "number", "v": {
            "kind": "binary_float", "format": "f64",
            "bits": list(struct.pack(">d", value)),
        }}
    if kind is str:
        value.encode("utf-8", errors="strict")
        return {"t": "text", "v": {"utf8": value, "encoding": "utf-8"}}
    if kind not in (list, dict):
        raise TypeError(f"morphism.unsupported-native: {path}: {kind.__module__}.{kind.__qualname__}")
    if seen is None:
        seen = set()
    identity = id(value)
    if identity in seen:
        raise TypeError(f"morphism.identity-required: shared or cyclic object at {path}")
    seen.add(identity)
    if kind is list:
        return {"t": "list", "v": [
            _lossless_native_witness(item, f"{path}[{index}]", seen, depth + 1)
            for index, item in enumerate(value)
        ]}
    if any(type(key) is not str for key in value):
        raise TypeError(f"morphism.non-string-map-key: {path}")
    return {"t": "map", "v": {
        key: _lossless_native_witness(item, f"{path}.{key}", seen, depth + 1)
        for key, item in value.items()
    }}


def _lossless_input(value, path="$", depth=0):
    """Admit the wire carrier before conversion, including callback ingress."""
    if depth > 64 or type(value) is not dict:
        raise TypeError(f"morphism.invalid-input: {path}")
    tag = value.get("t")
    if tag == "null":
        result = None
    elif tag == "bool" and type(value.get("v")) is bool:
        result = value["v"]
    elif tag == "number":
        number = value.get("v", {})
        if (number.get("kind") == "int"
                or (number.get("kind") == "binary_float" and number.get("format") == "f64")):
            result = oval_number_to_py(number)
        else:
            raise TypeError(f"morphism.unsupported-number: {path}")
    elif tag == "text" and value.get("v", {}).get("encoding") == "utf-8":
        result = value["v"]["utf8"]
    elif tag == "list" and type(value.get("v")) is list:
        result = [_lossless_input(item, f"{path}[{index}]", depth + 1)
                  for index, item in enumerate(value["v"])]
    elif tag == "map" and type(value.get("v")) is dict:
        result = {key: _lossless_input(item, f"{path}.{key}", depth + 1)
                  for key, item in value["v"].items()}
    else:
        raise TypeError(f"morphism.unsupported-input: {path}: {tag}")
    _lossless_native_witness(result, path)
    return result


def handle_exec_morphism(cmd):
    if cmd.get("contract") != _PLAIN_DATA_CONTRACT_V1:
        raise TypeError("morphism.unsupported-contract")
    request_id = cmd.get("request_id")
    if (type(request_id) is not str or len(request_id) != 64
            or any(char not in "0123456789abcdef" for char in request_id)):
        raise TypeError("morphism.invalid-request-id")
    handle_exec(cmd, morphism_contract=_PLAIN_DATA_CONTRACT_V1)


def handle_exec(cmd, morphism_contract=None):
    global _current_o_scope, _current_o_scope_wire, _active_morphism_contract
    code = cmd.get("code", "")
    bindings = cmd.get("bindings", {})

    # Validate every input before changing the actor or executing source.
    converted = {name: (_lossless_input(oval, f"$bindings.{name}")
                        if morphism_contract is not None else oval_to_py(oval))
                 for name, oval in bindings.items()}
    input_witnesses = ({name: _lossless_native_witness(value)
                        for name, value in converted.items()}
                       if morphism_contract is not None else None)
    if morphism_contract is not None and input_witnesses != bindings:
        raise TypeError("morphism.input-law-violation: native conversion changed the admitted OValue")
    _current_o_scope_wire = dict(bindings)
    _current_o_scope = {
        name: (_lossless_input(oval, f"$bindings.{name}")
               if morphism_contract is not None else oval_to_py(oval))
        for name, oval in bindings.items()
    }
    env.update(converted)

    def return_result(result):
        if morphism_contract is None:
            send_ok(result)
        else:
            value = _lossless_native_witness(result)
            write_wire_message({"status": "morphism_result_v1", "receipt": {
                "contract": morphism_contract,
                "request_id": cmd["request_id"],
                "input_witnesses": input_witnesses,
                "value": value,
            }}, _real_stdout.buffer)

    buf = io.StringIO()

    try:
        _active_morphism_contract = morphism_contract
        # Parse the whole code first.  If the last statement is a bare
        # expression (e.g. `6 * 7`, `type(q).__name__`), split it off so we
        # can `eval` it and capture its value — exec-mode silently discards
        # expression-statement values, which made `python^(6 * 7)_python`
        # return the empty string (the captured-stdout fallback) instead of
        # 42.  Anything that is genuinely a statement (assignments, defs,
        # loops, control flow) stays in the exec half and runs as before.
        # Python bodies inside .O (esp. inside indented HTML/MD literals) often
        # arrive with common leading whitespace. dedent so top-level Python
        # parses. Also strip surrounding blank lines (matches py impl).
        code = textwrap.dedent(code).strip("\n")

        module = ast.parse(code, mode="exec")

        trailing_expr = None
        if module.body and isinstance(module.body[-1], ast.Expr):
            tail = module.body[-1]
            module = ast.Module(body=module.body[:-1], type_ignores=[])
            trailing_expr = ast.Expression(body=tail.value)
            ast.copy_location(trailing_expr, tail)

        trailing_value = None
        with contextlib.redirect_stdout(buf):
            if module.body:
                exec(compile(module, "<O-python>", "exec"), env, env)
            if trailing_expr is not None:
                trailing_value = eval(
                    compile(trailing_expr, "<O-python>", "eval"), env, env
                )

        # Result-resolution priority:
        #   1. An explicit `__oval_result__ = ...` assignment (back-compat
        #      with every example in the repo that uses it).
        #   2. The value of a trailing expression — the new affordance.
        #   3. Captured stdout, for blocks that just `print(...)` for
        #      side-effect-as-value (preserves the prior fallback).
        #   4. Otherwise None (also covers a trailing literal `None`).
        if "__oval_result__" in env:
            result = env.pop("__oval_result__")
        elif trailing_value is not None:
            result = trailing_value
        elif buf.getvalue():
            result = buf.getvalue()
        else:
            result = None

        return_result(result)

    except SystemExit as e:
        # SystemExit inherits BaseException, not Exception, so it would slip
        # past the generic handler and terminate the shim process, causing
        # "backend closed stdout unexpectedly" on the Rust side.
        # Treat exit(0) as a clean null result; any other code as an error.
        code = e.code if e.code is not None else 0
        if code == 0:
            return_result(None)
        else:
            send_err(f"SystemExit({code})")

    except Exception:
        message = traceback.format_exc()
        dump_path = dump_generated_python(code if isinstance(code, str) else "")
        message += f"\nGenerated Python source: {dump_path}\n"
        send_err(message)
    finally:
        _active_morphism_contract = None

def handle_cleanup():
    global _current_o_scope, _current_o_scope_wire
    _current_o_scope = {}
    _current_o_scope_wire = {}
    _native_objects.clear()
    env.clear()
    env.update(_BASE_ENV)
    send_ok(None)


def _native_argument(value, depth=0):
    if depth > 64:
        raise ValueError("native.argument-depth: arguments exceed 64 levels")
    if type(value) is not dict:
        raise TypeError("native.invalid-argument: expected a typed OValue")
    tag = value.get("t")
    if tag == "native":
        return _native_objects.resolve(value)
    if tag == "list":
        return [_native_argument(item, depth + 1) for item in value["v"]]
    if tag == "map":
        return {key: _native_argument(item, depth + 1) for key, item in value["v"].items()}
    return _lossless_input(value, "$native_argument")


def handle_native_operation(cmd):
    global _active_native_operation
    previous = _active_native_operation
    request_id = cmd.get("request_id")
    try:
        if (type(request_id) is not str or len(request_id) != 64
                or any(character not in "0123456789abcdef" for character in request_id)):
            raise ValueError("native.invalid-request: expected a canonical request identity")
        if _active_morphism_contract is not None:
            raise TypeError("morphism.unsupported-native: native operations are outside plain data")
        operation = cmd.get("operation")
        arguments = cmd.get("arguments")
        if operation not in {"call", "get", "set", "release"} or type(arguments) is not list:
            raise ValueError("native.invalid-operation: expected call/get/set/release and arguments")
        arity = {"get": 1, "set": 2, "release": 0}.get(operation)
        if arity is not None and len(arguments) != arity:
            raise ValueError("native.invalid-arguments: incorrect operation arity")
        # Resolve/seal-check every handle and validate all data before running
        # a callable, a descriptor, setattr, or a release finalizer.
        retained = _native_objects.resolve(cmd.get("handle"))
        args = [_native_argument(value) for value in arguments]
        if operation in {"get", "set"} and type(args[0]) is not str:
            raise TypeError("native.invalid-attribute: attribute name must be text")
        _active_native_operation = True
        with contextlib.redirect_stdout(io.StringIO()):
            if operation == "call":
                result = retained(*args)
            elif operation == "get":
                result = getattr(retained, args[0])
            elif operation == "set":
                setattr(retained, args[0], args[1])
                result = None
            else:
                _native_objects.release(cmd["handle"])
                retained = None  # Run any finalizer before publishing the receipt.
                result = None
        if type(result) in {type(None), bool, int, str, float}:
            try:
                value = _lossless_native_witness(result)
            except (TypeError, ValueError):
                value = _native_objects.export(result)
        else:
            value = _native_objects.export(result)
    except BaseException as error:
        value = {"t": "error", "msg": f"native.operation-failed: {type(error).__name__}: {error}"}
    finally:
        _active_native_operation = previous
    write_wire_message({"status": "native_operation_result_v1", "request_id": request_id,
                        "value": value}, _real_stdout.buffer)

def handle_ping():
    send_ok(None)

command_loop(
    handle_exec,
    handle_cleanup=handle_cleanup,
    handle_ping=handle_ping,
    handle_state_capabilities=handle_state_capabilities,
    handle_checkpoint=handle_checkpoint,
    handle_restore=handle_restore,
    state_backend="python",
    handle_exec_morphism=handle_exec_morphism,
    handle_native_operation=handle_native_operation,
)
