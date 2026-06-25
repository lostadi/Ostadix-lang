#!/usr/bin/env python3
"""Backend shim for ruby^(...)_ruby blocks.

Executes code via the Ruby interpreter and captures stdout as the result.
"""
import sys
import json
import subprocess
import tempfile
import os
import traceback
from o_shim_common import read_wire_message, write_wire_message
from o_shim_common import stdout_result


def send_ok(value):
    write_wire_message({"status": "ok", "value": value})


def send_err(message):
    write_wire_message({"status": "err", "message": message})


def handle_exec(cmd):
    code = cmd.get("code", "")
    bindings = cmd.get("bindings", {})

    # Inject bindings as Ruby local variable assignments.
    preamble = ""
    for name, oval in bindings.items():
        t = oval.get("t")
        v = oval.get("v")
        if t == "str":
            escaped = v.replace("\\", "\\\\").replace('"', '\\"')
            preamble += f'{name} = "{escaped}"\n'
        elif t in ("int", "float"):
            preamble += f"{name} = {v}\n"
        elif t == "bool":
            preamble += f"{name} = {'true' if v else 'false'}\n"
        elif t == "null":
            preamble += f"{name} = nil\n"

    full_code = preamble + code

    try:
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".rb", delete=False
        ) as f:
            f.write(full_code)
            tmp = f.name

        try:
            result = subprocess.run(
                ["ruby", tmp],
                capture_output=True, text=True, timeout=60,
            )
        finally:
            os.unlink(tmp)

        if result.returncode != 0:
            stderr = result.stderr.strip()
            send_err(f"ruby exited with code {result.returncode}\n{stderr}")
        else:
            send_ok(stdout_result(result.stdout))
    except subprocess.TimeoutExpired:
        send_err("ruby execution timed out (60s)")
    except FileNotFoundError:
        send_err("ruby is not installed or not in PATH")
    except Exception:
        send_err(traceback.format_exc())


while True:
    try:
        cmd = read_wire_message()
        if cmd is None:
            break
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
