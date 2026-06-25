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
from o_shim_common import read_wire_message, write_wire_message

# Save a reference to the real process stdout (fd 1) before anything can
# redirect it. O.eval() must write eval_request directly over the IPC pipe
# even when the shim's handle_exec has temporarily redirected sys.stdout to
# a StringIO capture buffer for print() capture.
_real_stdout = sys.stdout
_current_o_scope = {}
_current_o_scope_wire = {}
_INT64_MIN = -(2 ** 63)
_INT64_MAX = 2 ** 63 - 1

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
        if scope_snapshot is not None:
            if not isinstance(scope_snapshot, OScopeValue):
                raise TypeError(
                    "O.eval explicit scope must be an OScopeValue from "
                    f"scope() or O.scope(), got {type(scope_snapshot).__name__!r}"
                )
            msg["scope"] = py_to_oval(scope_snapshot)
        write_wire_message(msg, _real_stdout.buffer)
        # Block until the runtime replies with eval_result.
        resp = read_wire_message(sys.stdin.buffer)
        if resp is None:
            raise RuntimeError("O.eval: runtime closed stdin before sending eval_result")
        if resp.get("cmd") != "eval_result":
            raise RuntimeError(
                f"O.eval: expected eval_result command, got {resp.get('cmd')!r}"
            )
        return oval_to_py(resp.get("value", {"t": "null"}))

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
            {k: oval_to_py(x) for k, x in wire_bindings.items()},
            wire_bindings,
        )
    if t == "blob":
        return base64.b64decode(v.get("v", ""))
    if t == "expr":
        return OExprValue(v.get("src", ""))

    return OOpaqueValue(v)


def oval_number_to_py(n):
    kind = n.get("kind")
    if kind == "int":
        return int(n.get("v", "0"))
    if kind == "rational":
        return fractions.Fraction(int(n.get("num", "0")), int(n.get("den", "1")))
    if kind == "decimal":
        special = n.get("special")
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
        return decimal.Decimal(int(n.get("coeff", "0"))).scaleb(int(n.get("exp10", 0)))
    if kind == "binary_float":
        bits = bytes(n.get("bits", []))
        if n.get("format") == "f32":
            return struct.unpack(">f", bits)[0]
        return struct.unpack(">d", bits)[0]
    if kind == "complex":
        return complex(
            oval_number_to_py(n.get("re", {"kind": "int", "v": "0"})),
            oval_number_to_py(n.get("im", {"kind": "int", "v": "0"})),
        )
    return OOpaqueValue({"t": "number", "v": n})


def py_number_to_oval_payload(x):
    if isinstance(x, int):
        return {"kind": "int", "v": str(x)}

    if isinstance(x, fractions.Fraction):
        return {"kind": "rational", "num": str(x.numerator), "den": str(x.denominator)}

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
        coeff = int("".join(str(digit) for digit in digits) or "0")
        if sign:
            coeff = -coeff
        return {"kind": "decimal", "coeff": str(coeff), "exp10": int(exponent), "special": None}

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

    if isinstance(x, dict):
        if all(isinstance(k, str) for k in x):
            return {"t": "map", "v": {k: py_to_oval(v) for k, v in x.items()}}
        return {
            "t": "entries_map",
            "entries": [[py_to_oval(k), py_to_oval(v)] for k, v in x.items()],
        }

    return {"t": "str", "v": str(x)}

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

def handle_exec(cmd):
    global _current_o_scope, _current_o_scope_wire
    code = cmd.get("code", "")
    bindings = cmd.get("bindings", {})

    _current_o_scope_wire = dict(bindings)
    _current_o_scope = {name: oval_to_py(oval) for name, oval in bindings.items()}
    for name, oval in bindings.items():
        env[name] = oval_to_py(oval)

    buf = io.StringIO()

    try:
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

        send_ok(result)

    except SystemExit as e:
        # SystemExit inherits BaseException, not Exception, so it would slip
        # past the generic handler and terminate the shim process, causing
        # "backend closed stdout unexpectedly" on the Rust side.
        # Treat exit(0) as a clean null result; any other code as an error.
        code = e.code if e.code is not None else 0
        if code == 0:
            send_ok(None)
        else:
            send_err(f"SystemExit({code})")

    except Exception:
        message = traceback.format_exc()
        dump_path = dump_generated_python(code if isinstance(code, str) else "")
        message += f"\nGenerated Python source: {dump_path}\n"
        send_err(message)

def handle_cleanup():
    global _current_o_scope, _current_o_scope_wire
    _current_o_scope = {}
    _current_o_scope_wire = {}
    env.clear()
    env["OHtml"] = OHtml
    env["OStorePath"] = OStorePath
    env["OExprValue"] = OExprValue
    env["OOpaqueValue"] = OOpaqueValue
    env["OScopeValue"] = OScopeValue
    env["O"] = O
    send_ok(None)

def handle_ping():
    send_ok(None)

while True:
    try:
        cmd = read_wire_message(sys.stdin.buffer)
        if cmd is None:
            break
        tag = cmd.get("cmd")

        if tag == "exec":
            handle_exec(cmd)
        elif tag == "cleanup":
            handle_cleanup()
        elif tag == "ping":
            handle_ping()
        else:
            send_err(f"unknown command: {tag!r}")

    except Exception:
        send_err(traceback.format_exc())
