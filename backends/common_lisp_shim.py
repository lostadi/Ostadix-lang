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


def send_ok(value):
    print(json.dumps({"status": "ok", "value": value}), flush=True)


def send_err(message):
    print(json.dumps({"status": "err", "message": message}), flush=True)


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
                        output = result.stdout
                        if output.endswith("\n"):
                            output = output[:-1]
                        send_ok({"t": "str", "v": output})
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
