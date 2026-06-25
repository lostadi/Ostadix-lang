#!/usr/bin/env python3
"""Backend shim for shell^(...)_shell blocks.

Executes code in a POSIX sh subprocess and captures stdout as the result.
"""
import sys
import json
import subprocess
import os
import traceback
from o_shim_common import stdout_result


def send_ok(value):
    print(json.dumps({"status": "ok", "value": value}), flush=True)


def send_err(message):
    print(json.dumps({"status": "err", "message": message}), flush=True)


def handle_exec(cmd):
    code = cmd.get("code", "")
    bindings = cmd.get("bindings", {})

    env = os.environ.copy()
    for name, oval in bindings.items():
        if oval.get("t") in ("str", "int", "float", "bool"):
            env[name] = str(oval.get("v", ""))

    try:
        result = subprocess.run(
            ["sh", "-c", code],
            capture_output=True, text=True, timeout=60, env=env,
        )
        if result.returncode != 0:
            stderr = result.stderr.strip()
            send_err(f"sh exited with code {result.returncode}\n{stderr}")
        else:
            send_ok(stdout_result(result.stdout))
    except subprocess.TimeoutExpired:
        send_err("sh execution timed out (60s)")
    except FileNotFoundError:
        send_err("sh is not installed or not in PATH")
    except Exception:
        send_err(traceback.format_exc())


for line in sys.stdin:
    try:
        cmd = json.loads(line)
        tag = cmd.get("cmd")

        if tag == "exec":
            handle_exec(cmd)
        elif tag == "cleanup":
            send_ok({"t": "null"})
        elif tag == "ping":
            send_ok({"t": "null"})
        else:
            send_err(f"unknown command: {tag!r}")

    except Exception:
        send_err(traceback.format_exc())
