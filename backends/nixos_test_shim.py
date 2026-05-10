#!/usr/bin/env python3
"""
NixOS test shim — Milestone E / F backend process for the Rust O evaluator.

Protocol: stdin/stdout JSON line protocol (same as all other shims).

exec command:
  code     — Nix attrset body with `nodes` and `testScript` keys.
  bindings — OValue bindings for $var splice resolution.

Returns OMap:
  {"t": "map", "v": {
    "success":    {"t": "bool",       "v": <bool>},
    "log":        {"t": "str",        "v": <str>},
    "store_path": {"t": "store_path", "path": <str>}
  }}
"""

import json
import os
import subprocess
import sys
import traceback

# Wrapper template: turns the user's attrset fragment into a full
# pkgs.testers.runNixOSTest call.  NIXPKGS_PATH may be overridden by env var.
_WRAPPER = """\
let
  pkgs = import ({nixpkgs}) {{}};
in
  pkgs.testers.runNixOSTest ({body})
"""


# ---------------------------------------------------------------------------
# OValue helpers
# ---------------------------------------------------------------------------

def send_ok(value):
    print(json.dumps({"status": "ok", "value": value}), flush=True)

def send_err(message):
    print(json.dumps({"status": "err", "message": message}), flush=True)

def oval_to_nix(v):
    """Render an OValue dict as a Nix expression fragment for $var splicing."""
    t = v.get("t")
    if t == "null":
        return "null"
    if t == "bool":
        return "true" if v.get("v") else "false"
    if t in ("int", "float"):
        return str(v.get("v"))
    if t in ("str", "html"):
        return json.dumps(v.get("v", ""))
    if t == "store_path":
        return json.dumps(v.get("path", ""))
    if t == "list":
        items = " ".join(oval_to_nix(x) for x in v.get("v", []))
        return f"[ {items} ]"
    if t == "map":
        pairs = " ".join(
            f"{k} = {oval_to_nix(val)};"
            for k, val in v.get("v", {}).items()
        )
        return f"{{ {pairs} }}"
    # Blob and other exotic types: stringify
    return json.dumps(str(v))


# ---------------------------------------------------------------------------
# Core: build and run a NixOS test
# ---------------------------------------------------------------------------

def run_nixos_test(code):
    nixpkgs = os.environ.get("NIXPKGS_PATH", "<nixpkgs>")
    expr = _WRAPPER.format(nixpkgs=nixpkgs, body=code)

    cmd = [
        "nix",
        "--extra-experimental-features", "nix-command",
        "build",
        "--no-link",
        "--print-out-paths",
        "--impure",
        "--expr",
        expr,
    ]

    completed = subprocess.run(
        cmd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=600,
    )

    if completed.returncode != 0:
        raise RuntimeError(
            f"nixos_test build failed (exit {completed.returncode}):\n"
            f"STDERR:\n{completed.stderr}\nSTDOUT:\n{completed.stdout}"
        )

    store_path = completed.stdout.strip().splitlines()[-1].strip()

    # Read the test log if present under the standard NixOS test output path.
    log_text = ""
    for candidate in ("test-output/log", "log"):
        candidate_path = os.path.join(store_path, candidate)
        try:
            with open(candidate_path) as fh:
                log_text = fh.read()
            break
        except OSError:
            continue
    else:
        log_text = completed.stderr or completed.stdout

    return {
        "t": "map",
        "v": {
            "success":    {"t": "bool",       "v": True},
            "log":        {"t": "str",        "v": log_text},
            "store_path": {"t": "store_path", "path": store_path},
        },
    }


# ---------------------------------------------------------------------------
# Command dispatch loop
# ---------------------------------------------------------------------------

def handle_exec(cmd):
    code     = cmd.get("code", "")
    bindings = cmd.get("bindings", {})

    # Apply variable splicing: replace each binding's repr in the code
    # string by rendering the OValue as a Nix expression fragment.
    for name, oval in bindings.items():
        code = code.replace(f"${name}", oval_to_nix(oval))

    try:
        result = run_nixos_test(code)
        send_ok(result)
    except Exception:
        send_err(traceback.format_exc())

def handle_ping():
    send_ok({"t": "null"})

def handle_cleanup():
    send_ok({"t": "null"})

for line in sys.stdin:
    try:
        cmd = json.loads(line)
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
