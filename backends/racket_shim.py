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


def send_ok(value):
    print(json.dumps({"status": "ok", "value": value}), flush=True)


def send_err(message):
    print(json.dumps({"status": "err", "message": message}), flush=True)


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
            output = result.stdout
            if output.endswith("\n"):
                output = output[:-1]
            send_ok({"t": "str", "v": output})
    except subprocess.TimeoutExpired:
        send_err("racket execution timed out (60s)")
    except FileNotFoundError:
        send_err("racket is not installed or not in PATH")
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
