#!/usr/bin/env python3
"""Backend shim for matlab^(...)_matlab blocks.

Executes code via GNU Octave (MATLAB-compatible open-source alternative)
or MATLAB if available. Captures stdout as the result.
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
        with tempfile.NamedTemporaryFile(
        ) as f:
            f.write(code)
            tmp = f.name

        try:
            if shutil.which("octave"):
                result = subprocess.run(
                    ["octave", "--no-gui", "--norc", "--silent", tmp],
                    capture_output=True, text=True, timeout=120,
                )
            elif shutil.which("matlab"):
                # MATLAB batch mode
                script_name = os.path.splitext(os.path.basename(tmp))[0]
                script_dir = os.path.dirname(tmp)
                result = subprocess.run(
                    [
                        "matlab", "-batch",
                        f"addpath('{script_dir}'); {script_name}",
                    ],
                    capture_output=True, text=True, timeout=300,
                )
            else:
                send_err(
                    "Neither GNU Octave nor MATLAB found in PATH. "
                    "Install Octave (https://octave.org) or MATLAB."
                )
                return
        finally:
            os.unlink(tmp)

        if result.returncode != 0:
            stderr = result.stderr.strip()
            send_err(f"MATLAB/Octave exited with code {result.returncode}\n{stderr}")
        else:
            send_ok(stdout_result(result.stdout))

    except subprocess.TimeoutExpired:
        send_err("MATLAB/Octave execution timed out")
    except Exception:
        send_err(traceback.format_exc())


command_loop(handle_exec)
