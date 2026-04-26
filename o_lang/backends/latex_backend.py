"""
LaTeX backend.

Semantics:
  * evaluate() returns OStr(body) -- body IS the LaTeX source.
  * render_child() produces LaTeX fragments. Strings get passed through
    (not escaped -- user can include \\LaTeX control sequences in Python
    strings if they want). Numbers render as-is. Blobs of image mime
    types are saved to a temp file and embedded with \\includegraphics;
    we return both the graphics directive and a preamble hint. For MVP
    we keep it simple and emit a base64 comment for unknown blobs.
"""

from __future__ import annotations

import base64
import json
import os
import tempfile
from typing import Any

from ..ovalue import (
    OBlob, OBool, OFloat, OInt, OList, OMap, ONull, OStr, OValue,
)


class LatexBackend:
    name = "latex"

    def make_env(self) -> Any:
        return None

    def render_child(self, v: OValue) -> str:
        if isinstance(v, ONull):
            return ""
        if isinstance(v, OBool):
            return r"\texttt{true}" if v.value else r"\texttt{false}"
        if isinstance(v, (OInt, OFloat)):
            return str(v.value)
        if isinstance(v, OStr):
            return v.value
        if isinstance(v, OBlob):
            if v.mime.startswith("image/"):
                ext = v.mime.split("/", 1)[1]
                ext = {"jpeg": "jpg", "svg+xml": "svg"}.get(ext, ext)
                tmp = tempfile.NamedTemporaryFile(
                    prefix="O_latex_", suffix=f".{ext}", delete=False,
                )
                tmp.write(v.data)
                tmp.close()
                return (
                    r"\includegraphics[width=\linewidth]{" + tmp.name + r"}"
                )
            return r"\texttt{[blob " + v.mime + r"]}"
        if isinstance(v, OList):
            inner = "".join(r"\item " + self.render_child(x) + "\n" for x in v.items)
            return "\\begin{itemize}\n" + inner + "\\end{itemize}"
        if isinstance(v, OMap):
            rows = "".join(
                r"\item \textbf{" + k + "}: " + self.render_child(val) + "\n"
                for k, val in v.pairs
            )
            return "\\begin{itemize}\n" + rows + "\\end{itemize}"
        return r"\texttt{" + json.dumps(v.to_json()) + r"}"

    def evaluate(self, body: str, env: Any) -> OValue:
        return OStr(body)
