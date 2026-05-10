#!/usr/bin/env python3
import sys
import json
import subprocess
import traceback

def py_json_to_oval(x):
    if x is None:
        return {"t": "null"}

    if isinstance(x, bool):
        return {"t": "bool", "v": x}

    if isinstance(x, int):
        return {"t": "int", "v": x}

    if isinstance(x, float):
        return {"t": "float", "v": x}

    if isinstance(x, str):
        return {"t": "str", "v": x}

    if isinstance(x, list):
        return {"t": "list", "v": [py_json_to_oval(i) for i in x]}

    if isinstance(x, dict):
        return {
            "t": "map",
            "v": {str(k): py_json_to_oval(v) for k, v in x.items()}
        }

    return {"t": "str", "v": str(x)}

def send_ok(value):
    print(json.dumps({"status": "ok", "value": value}), flush=True)

def send_err(message):
    print(json.dumps({"status": "err", "message": message}), flush=True)

def eval_nix_expr(code):
    cmd = [
        "nix",
        "--extra-experimental-features",
        "nix-command",
        "eval",
        "--json",
        "--impure",
        "--expr",
        code,
    ]

    completed = subprocess.run(
        cmd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    if completed.returncode != 0:
        raise RuntimeError(
            "nix eval failed\n\nSTDERR:\n"
            + completed.stderr
            + "\nSTDOUT:\n"
            + completed.stdout
        )

    return json.loads(completed.stdout)

def handle_exec(cmd):
    code = cmd.get("code", "")

    try:
        result = eval_nix_expr(code)
        send_ok(py_json_to_oval(result))
    except Exception:
        send_err(traceback.format_exc())

def handle_ping():
    send_ok({"t": "null"})

def handle_cleanup():
    send_ok({"t": "null"})

for line in sys.stdin:
    try:
        cmd = json.loads(line)
        tag = cmd.get("cmd")

        if tag == "exec":
            handle_exec(cmd)
        elif tag == "ping":
            handle_ping()
        elif tag == "cleanup":
            handle_cleanup()
        else:
            send_err(f"unknown command: {tag!r}")

    except Exception:
        send_err(traceback.format_exc())
