"""
OValue: the canonical intermediate value representation for .O

Every typed expression in O evaluates to an OValue. Inter-language data
passing MUST serialize through OValue -- this is the runtime embodiment of
the L* canonical form from the Transcompiler Composite Framework's T3
(Intersection) theorem.

Design principles:
  - Structurally rich enough to carry real data (sequences, maps, blobs).
  - Semantically minimal -- no methods, no inheritance, no callbacks.
    OValue is the extensional content of a value, not its behavior.
  - Self-describing via a tag. Every OValue knows what it is.
  - Homoiconic via OExpr: an OValue can carry an unevaluated O AST.
    This is what lets O code produce O code and evaluate it (Lisp-style).

Each language backend is responsible for implementing:
    serialize:   LangNativeValue -> OValue
    deserialize: OValue          -> LangNativeValue
    render:      OValue, target_language -> str

The `render` side is what lets a Python matplotlib figure become an
<img> tag when used as an atom inside an HTML expression -- the HTML
backend knows how to render an OBlob of mime "image/png" into HTML.
"""

from __future__ import annotations

import base64
import json
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional, Tuple, Union


# ---------------------------------------------------------------------------
# OValue tagged union
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class ONull:
    """The null / unit value. Produced when an expression has no return."""
    tag: str = "null"

    def to_json(self) -> Dict[str, Any]:
        return {"tag": "null"}


@dataclass(frozen=True)
class OBool:
    value: bool
    tag: str = "bool"

    def to_json(self) -> Dict[str, Any]:
        return {"tag": "bool", "value": self.value}


@dataclass(frozen=True)
class OInt:
    value: int
    tag: str = "int"

    def to_json(self) -> Dict[str, Any]:
        return {"tag": "int", "value": self.value}


@dataclass(frozen=True)
class OFloat:
    value: float
    tag: str = "float"

    def to_json(self) -> Dict[str, Any]:
        return {"tag": "float", "value": self.value}


@dataclass(frozen=True)
class OStr:
    value: str
    tag: str = "str"

    def to_json(self) -> Dict[str, Any]:
        return {"tag": "str", "value": self.value}


@dataclass(frozen=True)
class OList:
    items: Tuple["OValue", ...]
    tag: str = "list"

    def to_json(self) -> Dict[str, Any]:
        return {"tag": "list", "items": [v.to_json() for v in self.items]}


@dataclass(frozen=True)
class OMap:
    """Ordered key-value pairs. Keys must be OStr for simplicity in MVP."""
    pairs: Tuple[Tuple[str, "OValue"], ...]
    tag: str = "map"

    def get(self, key: str, default: Optional["OValue"] = None) -> Optional["OValue"]:
        for k, v in self.pairs:
            if k == key:
                return v
        return default

    def to_json(self) -> Dict[str, Any]:
        return {"tag": "map", "pairs": [[k, v.to_json()] for k, v in self.pairs]}


@dataclass(frozen=True)
class OBlob:
    """Opaque binary payload with a mime-type tag.

    This is the crucial constructor that lets a matplotlib figure become an
    <img> tag in HTML, or a LaTeX-compiled PDF become an embedded object in
    another document. The mime tag is the contract the receiving backend
    uses to decide how to render it.
    """
    data: bytes
    mime: str
    tag: str = "blob"

    def to_json(self) -> Dict[str, Any]:
        return {
            "tag": "blob",
            "mime": self.mime,
            "b64": base64.b64encode(self.data).decode("ascii"),
        }


@dataclass(frozen=True)
class OExpr:
    """An unevaluated O expression, carried as a value.

    This is what gives O meta-level homoiconicity: an O program can produce
    an O AST as a value, hand it to another expression, and have it
    evaluated. This is Lisp's quote/eval generalized across the multi-
    language system.

    We carry the AST reference as an arbitrary Python object so we don't
    create an import cycle with parser.py. The evaluator will type-check
    at eval time.
    """
    ast: Any                    # Really an ExpressionNode from parser.py
    tag: str = "expr"

    def to_json(self) -> Dict[str, Any]:
        return {"tag": "expr", "repr": repr(self.ast)}


OValue = Union[ONull, OBool, OInt, OFloat, OStr, OList, OMap, OBlob, OExpr]


# ---------------------------------------------------------------------------
# Python <-> OValue conversion helpers
# ---------------------------------------------------------------------------

def from_python(x: Any) -> OValue:
    """Best-effort lifting of a Python value into OValue.

    Backends will typically call this on the last-expression value returned
    by their interpreter when they don't have a more specific encoding.
    """
    # Already an OValue? Pass through unchanged so that Python code can
    # build up heterogeneous lists of OInt/OStr/OBlob without us stringifying
    # them on the way out.
    if isinstance(x, (ONull, OBool, OInt, OFloat, OStr, OList, OMap, OBlob, OExpr)):
        return x
    if x is None:
        return ONull()
    if isinstance(x, bool):         # must come before int -- bool is an int subclass
        return OBool(x)
    if isinstance(x, int):
        return OInt(x)
    if isinstance(x, float):
        return OFloat(x)
    if isinstance(x, str):
        return OStr(x)
    if isinstance(x, bytes):
        return OBlob(x, "application/octet-stream")
    if isinstance(x, (list, tuple)):
        return OList(tuple(from_python(v) for v in x))
    if isinstance(x, dict):
        return OMap(tuple((str(k), from_python(v)) for k, v in x.items()))
    # Fallback: stringify. A richer implementation would have registered
    # adapters here (e.g., matplotlib.figure.Figure -> OBlob("image/png")).
    return OStr(repr(x))


def to_python(v: OValue) -> Any:
    """Lower an OValue back into a Python native value."""
    if isinstance(v, ONull):
        return None
    if isinstance(v, (OBool, OInt, OFloat, OStr)):
        return v.value
    if isinstance(v, OList):
        return [to_python(item) for item in v.items]
    if isinstance(v, OMap):
        return {k: to_python(val) for k, val in v.pairs}
    if isinstance(v, OBlob):
        return v.data
    if isinstance(v, OExpr):
        return v.ast
    raise TypeError(f"Unknown OValue tag: {v!r}")


def to_json_str(v: OValue, indent: Optional[int] = 2) -> str:
    """Serialize an OValue to a JSON string for logging / debugging."""
    return json.dumps(v.to_json(), indent=indent)


# ---------------------------------------------------------------------------
# Generic "render to string" fallback
# ---------------------------------------------------------------------------

def render_plain(v: OValue) -> str:
    """Default rendering used when no backend-specific renderer applies.

    Scalars become their str(), lists/maps become JSON, blobs become a
    placeholder marker. Backends are free to override this for their own
    target language.
    """
    if isinstance(v, ONull):
        return ""
    if isinstance(v, (OBool, OInt, OFloat)):
        return str(v.value)
    if isinstance(v, OStr):
        return v.value
    if isinstance(v, OList):
        return "[" + ", ".join(render_plain(x) for x in v.items) + "]"
    if isinstance(v, OMap):
        return "{" + ", ".join(f"{k}: {render_plain(x)}" for k, x in v.pairs) + "}"
    if isinstance(v, OBlob):
        return f"<blob mime={v.mime} bytes={len(v.data)}>"
    if isinstance(v, OExpr):
        return f"<expr {v.ast!r}>"
    return str(v)
