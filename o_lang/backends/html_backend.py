"""
HTML backend.

Semantics:
  * evaluate() returns OHtml(body) -- HTML is a pure declarative language
    in the sense that "running" it just means surfacing the markup. OHtml
    marks the result as a trusted fragment that splices raw.
  * render_child() is where the interesting work happens: it knows how to
    embed arbitrary OValues from other languages AS HTML. An OBlob of
    mime image/png becomes a data URL <img>; a plain string becomes its
    HTML-escaped content (OHtml splices raw); a list becomes an HTML <ul>;
    a map becomes a <dl>. This is the backend
    that shows off the 'values from foreign languages can be consumed
    wherever an atom is expected' property of O.
"""

from __future__ import annotations

import base64
import html
import json
from typing import Any

from ..ovalue import (
    OBlob, OBool, OFloat, OHtml, OInt, OList, OMap, ONull, OStorePath, OStr,
    OValue,
)


class HtmlBackend:
    name = "html"

    def make_env(self) -> Any:
        return None  # stateless

    def render_child(self, v: OValue) -> str:
        if isinstance(v, ONull):
            return ""
        if isinstance(v, OBool):
            return "true" if v.value else "false"
        if isinstance(v, (OInt, OFloat)):
            return str(v.value)
        if isinstance(v, OHtml):
            # Trusted HTML fragment: splice raw.
            return v.value
        if isinstance(v, OStr):
            # Plain strings are untrusted text -- escape them. Trusted raw
            # HTML must arrive as OHtml (the "trusted HTML fragment" type
            # per SPEC.md), e.g. produced by an inner html^(...)_html block.
            return html.escape(v.value)
        if isinstance(v, OStorePath):
            return f'<code class="o-store-path">{html.escape(v.path)}</code>'
        if isinstance(v, OBlob):
            b64 = base64.b64encode(v.data).decode("ascii")
            if v.mime.startswith("image/"):
                return f'<img src="data:{v.mime};base64,{b64}" />'
            if v.mime == "text/html":
                return v.data.decode("utf-8", errors="replace")
            if v.mime.startswith("text/"):
                return html.escape(v.data.decode("utf-8", errors="replace"))
            # Generic binary: link to a data URL.
            return f'<a href="data:{v.mime};base64,{b64}">[blob {v.mime}, {len(v.data)} bytes]</a>'
        if isinstance(v, OList):
            items = "".join(f"<li>{self.render_child(x)}</li>" for x in v.items)
            return f"<ul>{items}</ul>"
        if isinstance(v, OMap):
            rows = "".join(
                f"<dt>{html.escape(k)}</dt><dd>{self.render_child(val)}</dd>"
                for k, val in v.pairs
            )
            return f"<dl>{rows}</dl>"
        # OExpr and unknown fall back to JSON dump
        return html.escape(json.dumps(v.to_json()))

    def evaluate(self, body: str, env: Any) -> OValue:
        # After child splicing, the body IS the HTML — a trusted fragment.
        return OHtml(body)
