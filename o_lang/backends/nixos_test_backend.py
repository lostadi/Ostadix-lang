"""
NixOS test backend — Milestones E and F.

Semantics:
  * evaluate() takes a Nix attrset containing `nodes` (machine configurations)
    and `testScript` (Python control script), wraps it in a
    `pkgs.testers.runNixOSTest` call, builds the test derivation with
    `nix build`, and returns the test result as an OMap containing:

      {
        "success":    OBool  — true iff all test assertions passed,
        "log":        OStr   — full test log (from the test driver),
        "store_path": OStorePath — /nix/store path of the test output,
      }

  * render_child reuses the Nix-syntax renderer so that values from prior
    nix^() or nix_store^() blocks can be spliced into test node configs.

  * The backend is stateless; each nixos_test^(...) block spawns fresh VMs.

Single-machine example (Milestone E):

    let lab = nixos_test^({
      nodes.machine = { pkgs, ... }: {
        services.nginx.enable = true;
      };
      testScript = ''
        machine.start()
        machine.wait_for_unit("nginx")
        machine.succeed("curl -s http://localhost | grep nginx")
      '';
    })_nixos_test

Two-machine example (Milestone F):

    let lab = nixos_test^({
      nodes.server = { pkgs, ... }: {
        services.nginx.enable = true;
      };
      nodes.client = { pkgs, ... }: {
        environment.systemPackages = [ pkgs.curl ];
      };
      testScript = ''
        server.start()
        client.start()
        server.wait_for_unit("nginx")
        result = client.succeed("curl -s http://server")
      '';
    })_nixos_test

Requirements: Nix with flakes/nix-command experimental features, KVM or
software-emulated QEMU, and <nixpkgs> on the NIX_PATH (or set
NIXPKGS_PATH env variable to an absolute path).
"""

from __future__ import annotations

import os
import subprocess
from typing import Any

from ..ovalue import OBool, OMap, ONull, OStorePath, OStr, OValue
from .nix_backend import _render_nix

# Wrapper that turns the user's attrset fragment into a full runNixOSTest call.
_NIX_TEST_WRAPPER = """\
let
  pkgs = import ({nixpkgs}) {{}};
in
  pkgs.testers.runNixOSTest ({body})
"""


class NixOSTestBackend:
    name = "nixos_test"

    def make_env(self) -> Any:
        return None  # stateless

    def render_child(self, v: OValue) -> str:
        return _render_nix(v)

    def evaluate(self, body: str, env: Any) -> OValue:
        nixpkgs = os.environ.get("NIXPKGS_PATH", "<nixpkgs>")
        expr = _NIX_TEST_WRAPPER.format(nixpkgs=nixpkgs, body=body)

        # Build the test derivation and capture the store path.
        build_cmd = [
            "nix",
            "--extra-experimental-features", "nix-command",
            "build",
            "--no-link",
            "--print-out-paths",
            "--impure",
            "--expr",
            expr,
        ]

        try:
            build = subprocess.run(
                build_cmd,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=600,  # VM tests can take minutes
            )
        except FileNotFoundError:
            raise RuntimeError(
                "nix executable not found. Install Nix to use nixos_test^(...)_nixos_test blocks."
            )

        if build.returncode != 0:
            raise RuntimeError(
                f"nixos_test build failed (exit {build.returncode}):\n"
                f"STDERR:\n{build.stderr}\nSTDOUT:\n{build.stdout}"
            )

        store_path = build.stdout.strip().splitlines()[-1].strip()

        # Read the test log if present.
        log_path = os.path.join(store_path, "test-output", "log")
        if not os.path.exists(log_path):
            log_path = os.path.join(store_path, "log")
        try:
            with open(log_path) as fh:
                log_text = fh.read()
            success = True
        except OSError:
            log_text = build.stderr or build.stdout
            # If we got here without error, treat as success.
            success = build.returncode == 0

        return OMap((
            ("success",    OBool(success)),
            ("log",        OStr(log_text)),
            ("store_path", OStorePath(store_path)),
        ))
