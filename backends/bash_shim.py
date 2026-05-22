#!/usr/bin/env python3
"""Backend shim for bash^(...)_bash blocks.

Executes the code body in a bash subprocess and returns its captured stdout
as an OStr. If the bash process exits with a non-zero status, an error is
returned with the combined stdout+stderr.
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
                    ["bash", "-c", code],
                    capture_output=True,
                    text=True,
                )
                if proc.returncode != 0:
                    combined = proc.stdout + proc.stderr
                    send_err(
                        f"bash exited with code {proc.returncode}:\n{combined}"
                    )
                else:
                    # Strip a single trailing newline that bash always appends;
                    # additional trailing newlines are preserved (intentional output).
                    out = proc.stdout
                    if out.endswith("\n"):
                        out = out[:-1]
                    send_ok({"t": "str", "v": out})
            except FileNotFoundError:
                send_err("bash executable not found; ensure bash is installed and on PATH")
        elif tag == "cleanup":
            send_ok({"t": "null"})
        elif tag == "ping":
            send_ok({"t": "null"})
        else:
            send_err(f"unknown command: {tag!r}")

    except Exception:
        send_err(traceback.format_exc())
