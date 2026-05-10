"""
Nix store backend — Milestone D.

Semantics:
  * evaluate() runs `nix eval --raw --impure --expr <body>` and validates
    that the result is a /nix/store/... path, then returns OStorePath(path).

  * Use this backend when you need a realized store path — a file or
    directory that nix^(...)_nix cannot give you directly because the value
    is a derivation or a path, not a JSON-representable Nix value.

Example:
    let hello_file = nix_store^(
      builtins.toFile "hello.txt" "Hello from O-lang + Nix\n"
    )_nix_store

    html^(<p>$hello_file</p>)_html

The backend is stateless; each nix_store^(...) block is a separate `nix eval`
call.  render_child reuses the Nix-syntax renderer from the nix backend so
that OValues from prior blocks can be spliced as Nix expressions.
"""

from __future__ import annotations

import subprocess
from typing import Any

from ..ovalue import OStorePath, OValue
from .nix_backend import _render_nix


class NixStoreBackend:
    name = "nix_store"

    def make_env(self) -> Any:
        return None  # stateless

    def render_child(self, v: OValue) -> str:
        return _render_nix(v)

    def evaluate(self, body: str, env: Any) -> OValue:
        cmd = [
            "nix",
            "--extra-experimental-features", "nix-command",
            "eval",
            "--raw",
            "--impure",
            "--expr",
            body,
        ]

        try:
            result = subprocess.run(
                cmd,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=60,
            )
        except FileNotFoundError:
            raise RuntimeError(
                "nix executable not found. Install Nix to use nix_store^(...)_nix_store blocks."
            )

        if result.returncode != 0:
            raise RuntimeError(
                f"nix eval --raw failed (exit {result.returncode}):\n"
                f"STDERR:\n{result.stderr}\nSTDOUT:\n{result.stdout}"
            )

        path = result.stdout.strip()
        if not path.startswith("/nix/store/"):
            raise RuntimeError(
                f"nix_store^(...) expression did not evaluate to a Nix store path.\n"
                f"Got: {path!r}\n"
                "Ensure the expression evaluates to a path value (e.g. builtins.toFile, "
                "a derivation outPath, or a package.outPath)."
            )

        return OStorePath(path)
