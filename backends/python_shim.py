#!/usr/bin/env python3
import sys
import json
import io
import contextlib
import base64
import traceback

class OHtml(str):
    """Typed trusted HTML fragment passed through O-lang."""
    def __new__(cls, value):
        return str.__new__(cls, value)

class OStorePath(str):
    """Typed Nix store path passed through O-lang."""
    def __new__(cls, value):
        return str.__new__(cls, value)

env = {"OHtml": OHtml, "OStorePath": OStorePath}

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

    if isinstance(x, str):
        return {"t": "str", "v": x}

    if isinstance(x, bytes):
        return {
            "t": "blob",
            "v": base64.b64encode(x).decode("ascii"),
            "mime": "application/octet-stream",
        }

    if isinstance(x, (list, tuple)):
        return {"t": "list", "v": [py_to_oval(i) for i in x]}

    if isinstance(x, dict):
        return {"t": "map", "v": {str(k): py_to_oval(v) for k, v in x.items()}}

    return {"t": "str", "v": str(x)}

def send_ok(value=None):
    print(json.dumps({"status": "ok", "value": py_to_oval(value)}), flush=True)

def send_err(message):
    print(json.dumps({"status": "err", "message": message}), flush=True)

def handle_exec(cmd):
    code = cmd.get("code", "")
    bindings = cmd.get("bindings", {})

    for name, oval in bindings.items():
        env[name] = oval_to_py(oval)

    buf = io.StringIO()

    try:
        with contextlib.redirect_stdout(buf):
            exec(code, env, env)

        if "__oval_result__" in env:
            result = env.pop("__oval_result__")
        else:
            result = buf.getvalue()

        send_ok(result)

    except Exception:
        send_err(traceback.format_exc())

def handle_cleanup():
    env.clear()
    env["OHtml"] = OHtml
    env["OStorePath"] = OStorePath
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
