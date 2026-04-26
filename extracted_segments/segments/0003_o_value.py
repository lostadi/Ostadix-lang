from __future__ import annotations
from dataclasses import dataclass
from typing import Any

@dataclass(frozen=True)
class ONull:
    def __repr__(self): return "null"

@dataclass(frozen=True)
class OBool:
    v: bool

@dataclass(frozen=True)
class OInt:
    v: int

@dataclass(frozen=True)
class OFloat:
    v: float

@dataclass(frozen=True)
class OStr:
    v: str

@dataclass(frozen=True)
class OBlob:
    data: bytes
    mime: str              # "image/png", "text/html", "application/pdf", ...

@dataclass
class OList:
    items: list[OValue]

@dataclass
class OMap:
    entries: dict[str, OValue]

OValue = ONull | OBool | OInt | OFloat | OStr | OBlob | OList | OMap

# ── Python-native <-> OValue ────────────────────────────────────────────────────

def py_to_oval(x: Any) -> OValue:
    match x:
        case None:              return ONull()
        case bool():            return OBool(x)
        case int():             return OInt(x)
        case float():           return OFloat(x)
        case str():             return OStr(x)
        case bytes():           return OBlob(x, "application/octet-stream")
        case list():            return OList([py_to_oval(i) for i in x])
        case dict():            return OMap({str(k): py_to_oval(v) for k,v in x.items()})
        case _:
            # Last resort: str() it
            return OStr(str(x))

def oval_to_py(v: OValue) -> Any:
    match v:
        case ONull():           return None
        case OBool(b):          return b
        case OInt(n):           return n
        case OFloat(f):         return f
        case OStr(s):           return s
        case OBlob(d, _):       return d
        case OList(items):      return [oval_to_py(i) for i in items]
        case OMap(entries):     return {k: oval_to_py(v) for k,v in entries.items()}

