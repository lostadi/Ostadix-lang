"""
O-lang: a type-directed operational semantics meta-language.

Public API:
  O_lang.run(src: str) -> OValue          -- parse + evaluate a .O string
  O_lang.parse(src: str) -> Document      -- parse only
  O_lang.evaluate_document(doc) -> OValue -- evaluate a parsed Document

See SPEC.md for the language definition.
"""

from .evaluator import EvalContext, evaluate_document, run
from .parser import Document, ExpressionNode, TextPart, parse, pretty
from .ovalue import (
    OBlob, OBool, OExpr, OFloat, OHtml, OInt, OList, OMap, ONull, OStorePath, OStr,
    OValue,
    from_python, render_plain, to_json_str, to_python,
)

__all__ = [
    "run", "parse", "evaluate_document", "EvalContext", "pretty",
    "Document", "ExpressionNode", "TextPart",
    "OValue", "ONull", "OBool", "OInt", "OFloat", "OStr", "OHtml", "OStorePath",
    "OList", "OMap", "OBlob", "OExpr",
    "from_python", "to_python", "render_plain", "to_json_str",
]

__version__ = "0.1.0"
