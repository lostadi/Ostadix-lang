#!/usr/bin/env python3
"""Backend shim for java^(...)_java blocks.

Compiles code with javac, runs it with java, and captures stdout.
The code must contain a class with a public static void main method.
"""
import sys
import json
import subprocess
import tempfile
import os
import re
import traceback
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from o_shim_common import read_wire_message, write_wire_message
from o_shim_common import stdout_result


def send_ok(value):
    write_wire_message({"status": "ok", "value": value})


def send_err(message):
    write_wire_message({"status": "err", "message": message})


def find_public_class(code):
    """Extract the public class name from Java source code."""
    m = re.search(r'\bpublic\s+class\s+(\w+)', code)
    if m:
        return m.group(1)
    # Fallback: find any class name
    m = re.search(r'\bclass\s+(\w+)', code)
    if m:
        return m.group(1)
    return "Main"


def handle_exec(cmd):
    code = cmd.get("code", "")

    try:
        class_name = find_public_class(code)

        with tempfile.TemporaryDirectory() as tmpdir:
            src = os.path.join(tmpdir, f"{class_name}.java")

            with open(src, "w") as f:
                f.write(code)

            # Compile
            comp = subprocess.run(
                ["javac", src],
                capture_output=True, text=True, timeout=120,
            )
            if comp.returncode != 0:
                stderr = comp.stderr.strip()
                send_err(f"javac compilation failed\n{stderr}")
                return

            # Run
            result = subprocess.run(
                ["java", "-cp", tmpdir, class_name],
                capture_output=True, text=True, timeout=60,
            )
            if result.returncode != 0:
                stderr = result.stderr.strip()
                send_err(f"java exited with code {result.returncode}\n{stderr}")
            else:
                send_ok(stdout_result(result.stdout))

    except subprocess.TimeoutExpired:
        send_err("Java compilation or execution timed out")
    except FileNotFoundError:
        send_err("javac/java is not installed or not in PATH")
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
