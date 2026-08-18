#!/usr/bin/env python3
"""Backend shim for mathematica^(...)_mathematica blocks.

Executes code via WolframScript (Wolfram Language / Mathematica CLI)
and captures stdout as the result.
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


def handle_exec(cmd):
    code = cmd.get("code", "")

    try:
        if shutil.which("wolframscript"):
            with tempfile.NamedTemporaryFile(
                mode="w", suffix=".wls", delete=False
            ) as f:
                f.write(code)
                tmp = f.name

            try:
                result = subprocess.run(
                    ["wolframscript", "-file", tmp],
                    capture_output=True, text=True, timeout=300,
                )
            finally:
                os.unlink(tmp)

            if result.returncode != 0:
                stderr = result.stderr.strip()
                send_err(f"wolframscript exited with code {result.returncode}\n{stderr}")
            else:
                send_ok(stdout_result(result.stdout))
        else:
            send_err(
                "wolframscript is not installed or not in PATH. "
                "Install Wolfram Engine (https://www.wolfram.com/engine/)."
            )
    except subprocess.TimeoutExpired:
        send_err("Mathematica execution timed out")
    except Exception:
        send_err(traceback.format_exc())


command_loop(handle_exec)
