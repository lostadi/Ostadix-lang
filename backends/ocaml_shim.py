#!/usr/bin/env python3
"""Backend shim for ocaml^(...)_ocaml blocks.

Executes code via the OCaml toplevel interpreter and captures stdout.
"""
import sys
import json
import subprocess
import tempfile
import os
import shutil
import traceback


def send_ok(value):
    print(json.dumps({"status": "ok", "value": value}), flush=True)


def send_err(message):
    print(json.dumps({"status": "err", "message": message}), flush=True)


def handle_exec(cmd):
    code = cmd.get("code", "")

    try:
        with tempfile.TemporaryDirectory() as tmpdir:
            src = os.path.join(tmpdir, "main.ml")
            with open(src, "w") as f:
                f.write(code)

            if shutil.which("ocaml"):
                # Interpreted execution via the toplevel
                result = subprocess.run(
                    ["ocaml", src],
                    capture_output=True, text=True, timeout=60,
                )
            elif shutil.which("ocamlfind") or shutil.which("ocamlopt"):
                # Compiled execution
                compiler = "ocamlopt" if shutil.which("ocamlopt") else "ocamlc"
                binary = os.path.join(tmpdir, "main")
                comp = subprocess.run(
                    [compiler, "-o", binary, src],
                    capture_output=True, text=True, timeout=120,
                )
                if comp.returncode != 0:
                    stderr = comp.stderr.strip()
                    send_err(f"{compiler} compilation failed\n{stderr}")
                    return

                result = subprocess.run(
                    [binary],
                    capture_output=True, text=True, timeout=60,
                )
            else:
                send_err("ocaml is not installed or not in PATH")
                return

            if result.returncode != 0:
                stderr = result.stderr.strip()
                send_err(f"OCaml exited with code {result.returncode}\n{stderr}")
            else:
                output = result.stdout
                if output.endswith("\n"):
                    output = output[:-1]
                send_ok({"t": "str", "v": output})

    except subprocess.TimeoutExpired:
        send_err("OCaml execution timed out")
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
