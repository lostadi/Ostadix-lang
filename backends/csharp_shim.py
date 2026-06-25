#!/usr/bin/env python3
"""Backend shim for csharp^(...)_csharp blocks.

Compiles code with the Mono C# compiler (mcs) or .NET SDK (dotnet),
runs the resulting executable, and captures stdout.
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


def try_mono(code, tmpdir):
    """Try compiling with mcs (Mono) and running with mono."""
    src = os.path.join(tmpdir, "Program.cs")
    binary = os.path.join(tmpdir, "Program.exe")
    with open(src, "w") as f:
        f.write(code)

    comp = subprocess.run(
        ["mcs", "-out:" + binary, src],
        capture_output=True, text=True, timeout=120,
    )
    if comp.returncode != 0:
        return comp

    return subprocess.run(
        ["mono", binary],
        capture_output=True, text=True, timeout=60,
    )


def try_dotnet_run(code, tmpdir):
    """Try running via 'dotnet run' with a temporary project."""
    # Create a minimal console project
    subprocess.run(
        ["dotnet", "new", "console", "--force", "-o", tmpdir],
        capture_output=True, text=True, timeout=60,
    )
    src = os.path.join(tmpdir, "Program.cs")
    with open(src, "w") as f:
        f.write(code)
    return subprocess.run(
        ["dotnet", "run", "--project", tmpdir],
        capture_output=True, text=True, timeout=120,
    )


def handle_exec(cmd):
    code = cmd.get("code", "")

    try:
        with tempfile.TemporaryDirectory() as tmpdir:
            result = None

            # Try available C# runtimes in order of preference.
            if shutil.which("dotnet"):
                try:
                    result = try_dotnet_run(code, tmpdir)
                except Exception:
                    pass

            if (result is None or result.returncode != 0) and shutil.which("mcs"):
                try:
                    result = try_mono(code, tmpdir)
                except Exception:
                    pass

            if result is None:
                send_err(
                    "No C# compiler found. Install .NET SDK (dotnet) or "
                    "Mono (mcs/mono) and ensure they are in PATH."
                )
                return

            if result.returncode != 0:
                stderr = result.stderr.strip()
                send_err(f"C# exited with code {result.returncode}\n{stderr}")
            else:
                send_ok(stdout_result(result.stdout))

    except subprocess.TimeoutExpired:
        send_err("C# compilation or execution timed out")
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
