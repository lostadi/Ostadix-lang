#!/usr/bin/env python3
"""Backend shim for racket^(...)_racket blocks.

Executes code via the Racket interpreter and captures stdout.
"""
import sys
import json
import subprocess
import tempfile
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

    try:
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".rkt", delete=False
        ) as f:
            f.write(code)
            tmp = f.name

        try:
            result = subprocess.run(
                ["racket", tmp],
                capture_output=True, text=True, timeout=60,
            )
        finally:
            os.unlink(tmp)

        if result.returncode != 0:
            stderr = result.stderr.strip()
            send_err(f"racket exited with code {result.returncode}\n{stderr}")
        else:
            send_ok(stdout_result(result.stdout))
    except subprocess.TimeoutExpired:
        send_err("racket execution timed out (60s)")
    except FileNotFoundError:
        send_err("racket is not installed or not in PATH")
    except Exception:
        send_err(traceback.format_exc())


command_loop(handle_exec)
