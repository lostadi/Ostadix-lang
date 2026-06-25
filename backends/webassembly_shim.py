#!/usr/bin/env python3
"""Backend shim for webassembly^(...)_webassembly blocks.

Executes WebAssembly text format (.wat) or binary (.wasm) via Wasmtime
or Wasmer and captures stdout as the result.

If the code is WAT (WebAssembly Text Format), it is first compiled to
.wasm using wat2wasm (from WABT) before execution.
"""
import sys
import json
import subprocess
import tempfile
import os
import shutil
import traceback
from o_shim_common import stdout_result


def send_ok(value):
    print(json.dumps({"status": "ok", "value": value}), flush=True)


def send_err(message):
    print(json.dumps({"status": "err", "message": message}), flush=True)


def is_wat(code):
    """Heuristic: WAT starts with '(module' after whitespace."""
    return code.lstrip().startswith("(module") or code.lstrip().startswith("(func")


def handle_exec(cmd):
    code = cmd.get("code", "")

    try:
        with tempfile.TemporaryDirectory() as tmpdir:
            if is_wat(code):
                wat_path = os.path.join(tmpdir, "module.wat")
                wasm_path = os.path.join(tmpdir, "module.wasm")

                with open(wat_path, "w") as f:
                    f.write(code)

                # Convert WAT to WASM
                if not shutil.which("wat2wasm"):
                    send_err(
                        "wat2wasm (from WABT toolkit) is not installed. "
                        "Install WABT (https://github.com/WebAssembly/wabt)."
                    )
                    return

                conv = subprocess.run(
                    ["wat2wasm", wat_path, "-o", wasm_path],
                    capture_output=True, text=True, timeout=30,
                )
                if conv.returncode != 0:
                    stderr = conv.stderr.strip()
                    send_err(f"wat2wasm failed\n{stderr}")
                    return
            else:
                # Assume raw WASM binary
                wasm_path = os.path.join(tmpdir, "module.wasm")
                with open(wasm_path, "wb") as f:
                    if isinstance(code, str):
                        f.write(code.encode("latin-1"))
                    else:
                        f.write(code)

            # Run with wasmtime or wasmer
            if shutil.which("wasmtime"):
                result = subprocess.run(
                    ["wasmtime", wasm_path],
                    capture_output=True, text=True, timeout=60,
                )
            elif shutil.which("wasmer"):
                result = subprocess.run(
                    ["wasmer", "run", wasm_path],
                    capture_output=True, text=True, timeout=60,
                )
            else:
                send_err(
                    "No WebAssembly runtime found. Install wasmtime "
                    "(https://wasmtime.dev) or wasmer (https://wasmer.io)."
                )
                return

            if result.returncode != 0:
                stderr = result.stderr.strip()
                send_err(f"wasm runtime exited with code {result.returncode}\n{stderr}")
            else:
                send_ok(stdout_result(result.stdout))

    except subprocess.TimeoutExpired:
        send_err("WebAssembly execution timed out")
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
