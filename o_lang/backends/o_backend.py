"""
The O^ backend: the host/sequencing language.

Semantics:
  * O^(...)_O evaluates its children IN ORDER via the EvalContext.
  * Side effects in child expressions (e.g., Python assignments to an env)
    happen in source order -- so state flows naturally along the reading
    order of the document.
  * The returned OValue is:
      - ONull   if no children evaluate to a meaningful value
      - the single child's OValue if exactly one
      - OList(child_values...)  if several
  * Text between child expressions is preserved as OStr children ONLY if
    it is non-whitespace. Pure whitespace is treated as formatting and
    dropped. This keeps `O^( ... )_O` readable without producing noisy
    empty-string children in the result list.

Why this matters:
  * The whole-document idiom is `O^(...)_O`, per Lee's convention. An O
    root collects all top-level parts of a document as a sequence of
    OValues, and the CLI knows how to concatenate them for any target
    language.
  * Because children are evaluated via the standard `_eval_expression`
    path, they still benefit from persistent envs, rich-value lifting,
    and all the cross-language machinery.
"""

from __future__ import annotations

from typing import Any, Dict, List

from ..ovalue import OList, ONull, OStr, OValue, render_plain


class OBackend:
    name = "O"

    def make_env(self) -> Dict[str, Any]:
        # The O env carries named bindings. Not wired up to $var splicing
        # yet (future work), but the slot exists so we can grow into it.
        return {"bindings": {}}

    def render_child(self, v: OValue) -> str:
        # If an O expression is used as a child of some other backend's
        # body, render it as its plain text. Most interesting cases are
        # when the O expression IS the root -- handled specially by the
        # CLI.
        if isinstance(v, OList):
            return "\n".join(render_plain(item) for item in v.items)
        return render_plain(v)

    def evaluate(self, body: str, env: Any, ctx=None) -> OValue:
        # Only reached if eval_ast is somehow skipped. Fall back to
        # returning the concatenated body as a string.
        return OStr(body)

    def eval_ast(self, node, ctx) -> OValue:
        """Take full control of children: evaluate each in source order."""
        # Deferred import avoids circular imports at module load time.
        from ..evaluator import _eval_expression
        from ..parser import ExpressionNode, TextPart

        # Materialize the env (even if unused) so the registry has a slot
        # for this (O, env_id) pair; useful when the user wants to ask
        # which bindings exist.
        _ = ctx.env_for("O", node.env_id)

        results: List[OValue] = []
        for child in node.body:
            if isinstance(child, TextPart):
                s = child.text
                if s.strip():
                    results.append(OStr(s))
                # whitespace-only text is formatting -- drop it
            elif isinstance(child, ExpressionNode):
                v = _eval_expression(child, ctx)
                if not isinstance(v, ONull):
                    results.append(v)
            else:
                raise TypeError(f"Unknown AST child node: {child!r}")

        if not results:
            return ONull()
        if len(results) == 1:
            return results[0]
        return OList(tuple(results))
