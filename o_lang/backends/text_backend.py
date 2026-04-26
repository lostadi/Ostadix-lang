"""Plain text backend -- used for `text^(...)_text` and as the O-root default.

Semantics: evaluate returns OStr(body). render_child uses the plain
fallback from ovalue.render_plain (scalars as str, blobs as placeholders).
"""

from __future__ import annotations

from typing import Any

from ..ovalue import OStr, OValue, render_plain


class TextBackend:
    name = "text"

    def make_env(self) -> Any:
        return None

    def render_child(self, v: OValue) -> str:
        return render_plain(v)

    def evaluate(self, body: str, env: Any) -> OValue:
        return OStr(body)
