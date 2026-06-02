#!/usr/bin/env python3
import sys
import json
import io
import ast
import contextlib
import base64
import traceback
import textwrap

# Save a reference to the real process stdout (fd 1) before anything can
# redirect it. O.eval() must write eval_request directly over the IPC pipe
# even when the shim's handle_exec has temporarily redirected sys.stdout to
# a StringIO capture buffer for print() capture.
_real_stdout = sys.stdout

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


class _OMod:
    """The ``O`` namespace injected into every Python block.

    Provides ``O.eval(q)`` for evaluating a quoted expression and
    ``O.quote(src)`` for constructing one from a source string.
    """

    @staticmethod
    def eval(q):
        """Evaluate a quoted expression and return its result.

        Sends an ``eval_request`` back to the Rust runtime, which evaluates
        the O source fragment and replies with an ``eval_result`` command.
        The function then returns the result as a Python value.

        Limitation: ``O.eval(q)`` cannot be used if ``q`` contains a
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
        msg = json.dumps({"status": "eval_request", "src": src}) + "\n"
        _real_stdout.write(msg)
        _real_stdout.flush()
        # Block until the runtime replies with eval_result.
        resp_line = sys.stdin.readline()
        if not resp_line:
            raise RuntimeError("O.eval: runtime closed stdin before sending eval_result")
        resp = json.loads(resp_line)
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
    if t == "str":
        return str(v.get("v"))
    if t == "html":
        return OHtml(v.get("v", ""))
    if t == "store_path":
        return OStorePath(v.get("path", ""))
    if t == "list":
        return [oval_to_py(x) for x in v.get("v", [])]
    if t == "map":
        return {k: oval_to_py(x) for k, x in v.get("v", {}).items()}
    if t == "blob":
        return base64.b64decode(v.get("v", ""))
    if t == "expr":
        return OExprValue(v.get("src", ""))

    raise ValueError(f"unknown OValue type: {t!r}")

def py_to_oval(x):
    if x is None:
        return {"t": "null"}

    if isinstance(x, bool):
        return {"t": "bool", "v": x}

    if isinstance(x, int):
        return {"t": "int", "v": x}

    if isinstance(x, float):
        return {"t": "float", "v": x}

    if isinstance(x, OHtml):
        return {"t": "html", "v": str(x)}

    if isinstance(x, OStorePath):
        return {"t": "store_path", "path": str(x)}

    if isinstance(x, OExprValue):
        return {"t": "expr", "src": x.src}

    if isinstance(x, str):
        return {"t": "str", "v": x}

    if isinstance(x, bytes):
        return {
            "t": "blob",
            "v": base64.b64encode(x).decode("ascii"),
            "mime": "application/octet-stream",
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

    if isinstance(x, (list, tuple)):
        return {"t": "list", "v": [py_to_oval(i) for i in x]}

    if isinstance(x, dict):
        return {"t": "map", "v": {str(k): py_to_oval(v) for k, v in x.items()}}

    return {"t": "str", "v": str(x)}

def send_ok(value=None):
    _real_stdout.write(json.dumps({"status": "ok", "value": py_to_oval(value)}) + "\n")
    _real_stdout.flush()

def send_err(message):
    _real_stdout.write(json.dumps({"status": "err", "message": message}) + "\n")
    _real_stdout.flush()

O = _OMod()
env = {
    "OHtml": OHtml,
    "OStorePath": OStorePath,
    "OExprValue": OExprValue,
    "O": O,
}

def handle_exec(cmd):
    code = cmd.get("code", "")
    bindings = cmd.get("bindings", {})

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

    except Exception:
        send_err(traceback.format_exc())

def handle_cleanup():
    env.clear()
    env["OHtml"] = OHtml
    env["OStorePath"] = OStorePath
    env["OExprValue"] = OExprValue
    env["O"] = O
    send_ok(None)

def handle_ping():
    send_ok(None)

for line in sys.stdin:
    try:
        cmd = json.loads(line)
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
