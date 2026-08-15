#!/usr/bin/env python3
"""Backend shim for common_lisp^(...)_common_lisp blocks.

Executes code via SBCL (Steel Bank Common Lisp) or another Common Lisp
implementation and captures stdout.
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
from o_shim_common import command_loop, write_wire_message
from o_shim_common import stdout_result


def send_ok(value):
    write_wire_message({"status": "ok", "value": value})


def send_err(message):
    write_wire_message({"status": "err", "message": message})


# Common Lisp implementations in order of preference.
CL_INTERPRETERS = [
    (["sbcl", "--script"], "sbcl"),
    (["ecl", "--shell"], "ecl"),
    (["clisp"], "clisp"),
    (["ccl", "--load"], "ccl"),
]


def handle_exec(cmd):
    code = cmd.get("code", "")

    try:
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".lisp", delete=False
        ) as f:
            f.write(code)
            tmp = f.name

        try:
            for argv_prefix, name in CL_INTERPRETERS:
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
                "No Common Lisp implementation found. Install SBCL, ECL, "
                "CLISP, or CCL and ensure it is in PATH."
            )
        finally:
            os.unlink(tmp)

    except subprocess.TimeoutExpired:
        send_err("Common Lisp execution timed out (60s)")
    except Exception:
        send_err(traceback.format_exc())


command_loop(handle_exec)
