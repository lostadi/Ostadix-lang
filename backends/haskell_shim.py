#!/usr/bin/env python3
"""Backend shim for haskell^(...)_haskell blocks.

Executes code via runghc (or ghc if runghc is unavailable) and captures stdout.
"""
import sys
import json
import subprocess
import tempfile
import os
import shutil
import traceback
from o_shim_common import read_wire_message, write_wire_message
from o_shim_common import stdout_result


def send_ok(value):
    write_wire_message({"status": "ok", "value": value})


def send_err(message):
    write_wire_message({"status": "err", "message": message})


def handle_exec(cmd):
    code = cmd.get("code", "")

    try:
        with tempfile.TemporaryDirectory() as tmpdir:
            src = os.path.join(tmpdir, "Main.hs")
            with open(src, "w") as f:
                f.write(code)

            if shutil.which("runghc"):
                # Interpreted execution
                result = subprocess.run(
                    ["runghc", src],
                    capture_output=True, text=True, timeout=120,
                )
            elif shutil.which("ghc"):
                # Compiled execution
                binary = os.path.join(tmpdir, "Main")
                comp = subprocess.run(
                    ["ghc", "-o", binary, src],
                    capture_output=True, text=True, timeout=120,
                )
                if comp.returncode != 0:
                    stderr = comp.stderr.strip()
                    send_err(f"ghc compilation failed\n{stderr}")
                    return

                result = subprocess.run(
                    [binary],
                    capture_output=True, text=True, timeout=60,
                )
            else:
                send_err("Neither runghc nor ghc found in PATH. Install GHC.")
                return

            if result.returncode != 0:
                stderr = result.stderr.strip()
                send_err(f"Haskell exited with code {result.returncode}\n{stderr}")
            else:
                send_ok(stdout_result(result.stdout))

    except subprocess.TimeoutExpired:
        send_err("Haskell execution timed out")
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
