"""
The `quote` backend: capture-without-evaluate.

`quote^( BODY )_quote` evaluates to a single OExpr that wraps the parsed
AST of BODY. Nothing inside is executed. This is the meta-level dual of
`O^`, which sequences its children by evaluating them. Together they
give .O Lisp-style homoiconicity generalized across languages:

    q = quote^(python^(2 + 2)_python)_quote      # q : OExpr
    python^(O.eval(q))_python                    # -> 4

Rules:
  * If BODY contains exactly one ExpressionNode and no other content,
    the quoted AST IS that node.
  * Otherwise BODY is wrapped in a synthetic O-node so `O.eval` treats
    it as a sequence (same convention as the top-level O^ wrapper).
  * Pure whitespace inside quote^ is treated as formatting and dropped.
    Non-whitespace text is preserved as TextPart so it round-trips
    through eval (e.g. quoting `O^(literal string)_O` works).

Why this is tiny:
  * `quote` doesn't need a runtime environment, a body string, or even
    a child-evaluation strategy. It's a structural operator: it just
    hands the AST back as an OValue.
"""

from __future__ import annotations

from typing import Any

from ..ovalue import OExpr, OValue


class QuoteBackend:
    name = "quote"

    def make_env(self) -> Any:
        return None

    def render_child(self, v: OValue) -> str:
        # If a quoted expression is spliced inside some other body as a
        # child, the sensible fallback is its repr. In practice, quote^
        # is almost always consumed by O.eval() inside a Python block,
        # which goes through python_backend.render_child instead.
        return repr(v)

    def evaluate(self, body: str, env: Any, ctx=None) -> OValue:
        # Defensive: if the structural hook is skipped we at least return
        # something self-describing rather than silently evaluating.
        from ..ovalue import OStr
        return OStr(body)

    def eval_ast(self, node, ctx) -> OValue:
        """Capture the body as an OExpr. DO NOT evaluate children."""
        # Deferred import avoids a circular import at module load time.
        from ..parser import ExpressionNode, TextPart

        # Trim whitespace-only TextParts so `quote^( python^(1)_python )_quote`
        # is equivalent to `quote^(python^(1)_python)_quote`.
        trimmed = []
        for child in node.body:
            if isinstance(child, TextPart) and not child.text.strip():
                continue
            trimmed.append(child)

        # Single ExpressionNode => quote that node directly.
        if len(trimmed) == 1 and isinstance(trimmed[0], ExpressionNode):
            return OExpr(trimmed[0])

        # Multiple children or mixed text: wrap in a synthetic O-node so
        # `O.eval` treats it as a sequence (identical to the top-level O^
        # wrapper convention).
        synthetic = ExpressionNode(
            language="O",
            env_id=node.env_id,
            env_explicit=False,
            body=trimmed,
        )
        return OExpr(synthetic)
