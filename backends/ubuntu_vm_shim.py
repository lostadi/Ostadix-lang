#!/usr/bin/env python3
"""Backend shim for ubuntu_vm^(...)_ubuntu_vm blocks.

Executes code inside a persistent Ubuntu VM via Multipass.
"""
import sys
import os
import subprocess
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from o_shim_common import read_wire_message, write_wire_message
from o_shim_common import stdout_result

VM_NAME = "ostadix-vm"

def send_ok(value):
    write_wire_message({"status": "ok", "value": value})

def send_err(message):
    write_wire_message({"status": "err", "message": message})

def ensure_vm():
    # Only verify/launch once per process
    if hasattr(ensure_vm, "ready"):
        return
    res = subprocess.run(["multipass", "info", VM_NAME], capture_output=True)
    if res.returncode != 0:
        # Need to launch
        subprocess.run(["multipass", "launch", "--name", VM_NAME], capture_output=True, check=True)
    else:
        # Check if running
        if b"Running" not in res.stdout:
            subprocess.run(["multipass", "start", VM_NAME], capture_output=True, check=True)
    ensure_vm.ready = True

def handle_exec(cmd):
    code = cmd.get("code", "")
    bindings = cmd.get("bindings", {})

    try:
        ensure_vm()
    except Exception as e:
        send_err(f"Failed to provision/start Multipass VM: {e}")
        return

    # Build env string for bindings
    env_exports = []
    for name, oval in bindings.items():
        if oval.get("t") in ("str", "int", "float", "bool"):
            val = str(oval.get("v", "")).replace("'", "'\\''")
            env_exports.append(f"export {name}='{val}'")
    
    env_setup = "\n".join(env_exports)
    
    # Run the script inside the VM
    # multipass exec reads from stdin if we pass no command or pipe to bash
    script = f"{env_setup}\n{code}"
    
    try:
        result = subprocess.run(
            ["multipass", "exec", VM_NAME, "--", "bash"],
            input=script, capture_output=True, text=True, timeout=120
        )
        if result.returncode != 0:
            stderr = result.stderr.strip()
            send_err(f"ubuntu_vm exited with code {result.returncode}\n{stderr}")
        else:
            send_ok(stdout_result(result.stdout))
    except subprocess.TimeoutExpired:
        send_err("ubuntu_vm execution timed out (120s)")
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
