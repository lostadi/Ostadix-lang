#!/usr/bin/env python3
import sys
import json
import subprocess
import traceback
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from o_shim_common import read_wire_message, write_wire_message
from o_shim_common import json_value_to_oval


def send_ok(value):
    write_wire_message({"status": "ok", "value": value})

def send_err(message):
    write_wire_message({"status": "err", "message": message})

def eval_nix_expr(code):
    cmd = [
        "nix",
        "--extra-experimental-features",
        "nix-command",
        "eval",
        "--json",
        "--impure",
        "--expr",
        code,
    ]

    completed = subprocess.run(
        cmd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    if completed.returncode != 0:
        raise RuntimeError(
            "nix eval failed\n\nSTDERR:\n"
            + completed.stderr
            + "\nSTDOUT:\n"
            + completed.stdout
        )

    return json.loads(completed.stdout)

def handle_exec(cmd):
    code = cmd.get("code", "")

    try:
        result = eval_nix_expr(code)
        send_ok(json_value_to_oval(result))
    except Exception:
        send_err(traceback.format_exc())

def handle_ping():
    send_ok({"t": "null"})

def handle_cleanup():
    send_ok({"t": "null"})

while True:
    try:
        cmd = read_wire_message()
        if cmd is None:
            break
        tag = cmd.get("cmd")

        if tag == "exec":
            handle_exec(cmd)
        elif tag == "ping":
            handle_ping()
        elif tag == "cleanup":
            handle_cleanup()
        else:
            send_err(f"unknown command: {tag!r}")

    except Exception:
        send_err(traceback.format_exc())
