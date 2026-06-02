#!/usr/bin/env python3
"""Backend shim for cpp^(...)_cpp blocks.

Compiles code with g++ (C++17), runs the resulting binary, and captures stdout.
"""
import sys
import json
import subprocess
import tempfile
import os
import traceback


def send_ok(value):
    print(json.dumps({"status": "ok", "value": value}), flush=True)


def send_err(message):
    print(json.dumps({"status": "err", "message": message}), flush=True)


def handle_exec(cmd):
    code = cmd.get("code", "")

    try:
        with tempfile.TemporaryDirectory() as tmpdir:
            src = os.path.join(tmpdir, "main.cpp")
            binary = os.path.join(tmpdir, "main")

            with open(src, "w") as f:
                f.write(code)

            # Compile with C++17
            comp = subprocess.run(
                ["g++", "-std=c++17", "-o", binary, src],
                capture_output=True, text=True, timeout=120,
            )
            if comp.returncode != 0:
                stderr = comp.stderr.strip()
                send_err(f"g++ compilation failed\n{stderr}")
                return

            # Run
            result = subprocess.run(
                [binary],
                capture_output=True, text=True, timeout=60,
            )
            if result.returncode != 0:
                stderr = result.stderr.strip()
                send_err(f"C++ program exited with code {result.returncode}\n{stderr}")
            else:
                output = result.stdout
                if output.endswith("\n"):
                    output = output[:-1]
                send_ok({"t": "str", "v": output})

    except subprocess.TimeoutExpired:
        send_err("C++ compilation or execution timed out")
    except FileNotFoundError:
        send_err("g++ is not installed or not in PATH")
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
