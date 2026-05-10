#!/usr/bin/env python3
import sys
import json
import subprocess
import traceback

def send_ok_store_path(path):
    print(json.dumps({
        "status": "ok",
        "value": {"t": "store_path", "path": path}
    }), flush=True)

def send_ok_null():
    print(json.dumps({
        "status": "ok",
        "value": {"t": "null"}
    }), flush=True)

def send_err(message):
    print(json.dumps({"status": "err", "message": message}), flush=True)

def eval_store_path(code):
    cmd = [
        "nix",
        "--extra-experimental-features",
        "nix-command",
        "eval",
        "--raw",
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
            "nix eval --raw failed\n\nSTDERR:\n"
            + completed.stderr
            + "\nSTDOUT:\n"
            + completed.stdout
        )

    path = completed.stdout.strip()

    if not path.startswith("/nix/store/"):
        raise RuntimeError(f"expression did not evaluate to a Nix store path: {path!r}")

    return path

def handle_exec(cmd):
    code = cmd.get("code", "")

    try:
        path = eval_store_path(code)
        send_ok_store_path(path)
    except Exception:
        send_err(traceback.format_exc())

for line in sys.stdin:
    try:
        cmd = json.loads(line)
        tag = cmd.get("cmd")

        if tag == "exec":
            handle_exec(cmd)
        elif tag == "ping":
            send_ok_null()
        elif tag == "cleanup":
            send_ok_null()
        else:
            send_err(f"unknown command: {tag!r}")

    except Exception:
        send_err(traceback.format_exc())
