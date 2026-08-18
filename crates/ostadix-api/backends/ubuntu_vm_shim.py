#!/usr/bin/env python3
"""Backend shim for ubuntu_vm^(...)_ubuntu_vm blocks.

Executes code inside a persistent Ubuntu VM via Multipass.
"""
import sys
import base64
import hashlib
import os
import subprocess
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from o_shim_common import (
    StatePinRequired,
    admitted_tool_path,
    command_loop,
    make_checkpoint,
    state_capabilities,
    validate_checkpoint,
    write_wire_message,
)
from o_shim_common import stdout_result

UBUNTU_RESOURCE_CODEC_V1 = "ostadix.multipass-resource/v1"


def _session_identity():
    identity = os.environ.get("O_BACKEND_SESSION_ID", "")
    if len(identity) == 64 and all(character in "0123456789abcdefABCDEF" for character in identity):
        return identity.lower()
    # Direct/manual shim launches do not have the registry identity. Keep even
    # those isolated instead of silently sharing the historical global VM.
    return hashlib.sha256(
        f"ostadix-manual-ubuntu-session/v1\0{os.getpid()}".encode("utf-8")
    ).hexdigest()


SESSION_ID = _session_identity()
VM_NAME = "ostadix-" + base64.b32encode(bytes.fromhex(SESSION_ID)).decode("ascii").lower().rstrip("=")

def multipass_command():
    return admitted_tool_path("multipass")

def send_ok(value):
    write_wire_message({"status": "ok", "value": value})

def send_err(message):
    write_wire_message({"status": "err", "message": message})

def ensure_vm():
    # Only verify/launch once per process
    if hasattr(ensure_vm, "ready"):
        return
    res = subprocess.run([multipass_command(), "info", VM_NAME], capture_output=True)
    if res.returncode != 0:
        # Need to launch
        subprocess.run([multipass_command(), "launch", "--name", VM_NAME], capture_output=True, check=True)
    else:
        # Check if running
        if b"Running" not in res.stdout:
            subprocess.run([multipass_command(), "start", VM_NAME], capture_output=True, check=True)
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
            [multipass_command(), "exec", VM_NAME, "--", "bash"],
            input=script, capture_output=True, text=True, timeout=600
        )
        if result.returncode != 0:
            stderr = result.stderr.strip()
            send_err(f"ubuntu_vm exited with code {result.returncode}\n{stderr}")
        else:
            send_ok(stdout_result(result.stdout))
    except subprocess.TimeoutExpired:
        send_err("ubuntu_vm execution timed out (600s)")
    except Exception:
        send_err(traceback.format_exc())


def handle_state_capabilities():
    return state_capabilities(
        "ubuntu_vm", "external_pinned", UBUNTU_RESOURCE_CODEC_V1, False
    )


def handle_checkpoint(max_bytes):
    return make_checkpoint(
        "ubuntu_vm",
        "external_pinned",
        UBUNTU_RESOURCE_CODEC_V1,
        {
            "profile": "multipass-resource-manifest-only",
            "session_id": SESSION_ID,
            "vm_name": VM_NAME,
            "provider": "multipass-local",
        },
        external_resources=[{
            "kind": "multipass-instance",
            "identity": f"multipass-local:{VM_NAME}",
            "recovery": "same-live-provider-resource-required",
            "metadata": {
                "provider": "multipass-local",
                "session_id": SESSION_ID,
                "vm_name": VM_NAME,
            },
        }],
    )


def handle_restore(checkpoint):
    validate_checkpoint(checkpoint)
    raise StatePinRequired(
        "$external_resources[0]",
        "Ubuntu VM state remains in the named live Multipass instance; portable restore is unsupported",
    )


command_loop(
    handle_exec,
    handle_state_capabilities=handle_state_capabilities,
    handle_checkpoint=handle_checkpoint,
    handle_restore=handle_restore,
    state_backend="ubuntu_vm",
)
