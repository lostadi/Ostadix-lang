#!/usr/bin/env python3
"""Backend shim for shell^(...)_shell blocks.

Executes the code body using /bin/sh and returns its captured stdout as an
OStr. If the shell process exits with a non-zero status, an error is returned
with the combined stdout+stderr.
"""
import sys
import json
import subprocess
import traceback


def send_ok(value):
    print(json.dumps({"status": "ok", "value": value}), flush=True)


def send_err(message):
    print(json.dumps({"status": "err", "message": message}), flush=True)


for line in sys.stdin:
    try:
        cmd = json.loads(line)
        tag = cmd.get("cmd")

        if tag == "exec":
            code = cmd.get("code", "")
            try:
                proc = subprocess.run(
                    ["/bin/sh", "-c", code],
                    capture_output=True,
                    text=True,
                )
                if proc.returncode != 0:
                    combined = proc.stdout + proc.stderr
                    send_err(
                        f"sh exited with code {proc.returncode}:\n{combined}"
                    )
                else:
                    out = proc.stdout
                    if out.endswith("\n"):
                        out = out[:-1]
                    send_ok({"t": "str", "v": out})
            except FileNotFoundError:
                send_err("/bin/sh not found; ensure a POSIX shell is available")
        elif tag == "cleanup":
            send_ok({"t": "null"})
        elif tag == "ping":
            send_ok({"t": "null"})
        else:
            send_err(f"unknown command: {tag!r}")

    except Exception:
        send_err(traceback.format_exc())
