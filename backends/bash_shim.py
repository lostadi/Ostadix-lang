#!/usr/bin/env python3
"""Stub backend shim for bash^(...)_bash blocks.

This is a placeholder. It returns the code text as an OStr so that .O
files containing bash^ blocks at least parse and evaluate without crashing
the runtime. Replace this with a real bash-execution shim when ready.

To implement for real: exec the code in a subprocess bash shell, capture
stdout as the result.
"""
import sys
import json
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
            # Stub: return the code text unchanged.
            code = cmd.get("code", "")
            send_ok({"t": "str", "v": code})
        elif tag == "cleanup":
            send_ok({"t": "null"})
        elif tag == "ping":
            send_ok({"t": "null"})
        else:
            send_err(f"unknown command: {tag!r}")

    except Exception:
        send_err(traceback.format_exc())
