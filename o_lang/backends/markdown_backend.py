"""
Markdown backend.

Semantics:
  * evaluate() returns OStr(body) -- the markdown source with children
    already spliced in.
  * render_child() produces markdown fragments. An OBlob image/png becomes
    ![](data:image/png;base64,...); strings pass through; lists become
    bullet lists.

The CLI's final render step can convert markdown -> HTML if --as html is
requested; otherwise the raw markdown is the output.
"""

from __future__ import annotations

import base64
import json
from typing import Any

from ..ovalue import (
    OBlob, OBool, OFloat, OInt, OList, OMap, ONull, OStr, OValue,
)


class MarkdownBackend:
    name = "markdown"

    def make_env(self) -> Any:
        return None

    def render_child(self, v: OValue) -> str:
        if isinstance(v, ONull):
            return ""
        if isinstance(v, OBool):
            return "true" if v.value else "false"
        if isinstance(v, (OInt, OFloat)):
            return str(v.value)
        if isinstance(v, OStr):
            return v.value
        if isinstance(v, OBlob):
            b64 = base64.b64encode(v.data).decode("ascii")
            if v.mime.startswith("image/"):
                return f"![](data:{v.mime};base64,{b64})"
            if v.mime.startswith("text/"):
                return v.data.decode("utf-8", errors="replace")
            return f"[blob {v.mime}, {len(v.data)} bytes]"
        if isinstance(v, OList):
            # Inline comma list keeps sub-expression splicing readable when
            # it lands mid-sentence. Users who want a real bullet list should
            # build the markdown string themselves in their Python block.
            return "[" + ", ".join(self.render_child(x) for x in v.items) + "]"
        if isinstance(v, OMap):
            return "{" + ", ".join(
                f"{k}: {self.render_child(val)}" for k, val in v.pairs
            ) + "}"
        return f"`{json.dumps(v.to_json())}`"

    def evaluate(self, body: str, env: Any) -> OValue:
        return OStr(body)
