#!/usr/bin/env python3
"""Backend shim for lisp^(...)_lisp blocks.

Executes code via a Scheme interpreter (Guile, Chicken, or Chez Scheme)
and captures stdout. For Common Lisp, use common_lisp^ instead.
"""
import sys
import json
import subprocess
import tempfile
import os
import shutil
import traceback
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from o_shim_common import read_wire_message, write_wire_message
from o_shim_common import stdout_result


def send_ok(value):
    write_wire_message({"status": "ok", "value": value})


def send_err(message):
    write_wire_message({"status": "err", "message": message})


# Scheme interpreters in order of preference.
SCHEME_INTERPRETERS = [
    (["guile", "--no-auto-compile", "-s"], "guile"),
    (["csi", "-s"], "chicken"),
    (["chez", "--program"], "chez"),
    (["scheme", "--program"], "scheme"),
]


def handle_exec(cmd):
    code = cmd.get("code", "")

    try:
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".scm", delete=False
        ) as f:
            f.write(code)
            tmp = f.name

        try:
            for argv_prefix, name in SCHEME_INTERPRETERS:
                if shutil.which(argv_prefix[0]):
                    result = subprocess.run(
                        argv_prefix + [tmp],
                        capture_output=True, text=True, timeout=60,
                    )
                    if result.returncode != 0:
                        stderr = result.stderr.strip()
                        send_err(f"{name} exited with code {result.returncode}\n{stderr}")
                    else:
                        send_ok(stdout_result(result.stdout))
                    return

            send_err(
                "No Scheme interpreter found. Install Guile, Chicken, or "
                "Chez Scheme and ensure it is in PATH."
            )
        finally:
            os.unlink(tmp)

    except subprocess.TimeoutExpired:
        send_err("lisp execution timed out (60s)")
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
