#!/usr/bin/env python3
"""Backend shim for shell^(...)_shell blocks.

Executes code in a POSIX sh subprocess and captures stdout as the result.
"""
import sys
import json
import subprocess
import os
import traceback
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from o_shim_common import command_loop, write_wire_message
from o_shim_common import stdout_result


def send_ok(value):
    write_wire_message({"status": "ok", "value": value})


def send_err(message):
    write_wire_message({"status": "err", "message": message})


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


command_loop(handle_exec)
