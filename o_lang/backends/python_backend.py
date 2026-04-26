"""
Python backend: real execution, persistent globals per env_id.

Semantics of evaluate(body, env):
  * The body string is treated as a Python module top-level.
  * If the body's final statement is an expression, its value becomes
    the expression's OValue. Otherwise, captured stdout becomes an OStr;
    if there's no stdout either, the result is ONull.
  * `env` is the persistent globals dict for this (python, env_id) pair.
    State (imports, variables, function defs) survives across invocations
    of the same env_id, which is how you get a REPL-like shell per [N].

Opportunistic native -> OValue lifting:
  * matplotlib.figure.Figure  ->  OBlob(png_bytes, 'image/png')
  * PIL.Image.Image           ->  OBlob(png_bytes, 'image/png')
  * bytes                     ->  OBlob(bytes, 'application/octet-stream')
  * everything else           ->  ovalue.from_python(x)

A helper module `O` is injected into every Python env, giving user code
explicit constructors when they need precise OValue control:
    O.blob(data, mime)   ->  OBlob
    O.ret(value)         ->  explicit return value (bypasses last-expr logic)
"""

from __future__ import annotations

import ast
import io
import sys
import textwrap
from typing import Any, Dict, List

from ..ovalue import (
    OBlob, OBool, OExpr, OFloat, OInt, OList, OMap, ONull, OStr, OValue,
    from_python, to_python,
)


# ---------------------------------------------------------------------------
# The `O` helper module exposed to user Python code
# ---------------------------------------------------------------------------

class _OHelpers:
    """Minimal runtime exposed as `O` inside Python exprs.

    Note that `_OHelpers` instances are per-call: when the PythonBackend
    runs an expression it instantiates a fresh helper bound to the live
    EvalContext so `O.eval(expr)` can walk back into the evaluator.
    """

    def __init__(self, ctx=None):
        # ctx is the live EvalContext, so we can re-enter the evaluator
        # when the user calls O.eval(expr). It's optional for back-compat
        # (e.g., someone manually calling PythonBackend.evaluate without
        # threading a ctx through).
        self._ctx = ctx

    @staticmethod
    def blob(data: bytes, mime: str = "application/octet-stream") -> OBlob:
        return OBlob(bytes(data), mime)

    @staticmethod
    def str(s: str) -> OStr:
        return OStr(str(s))

    @staticmethod
    def ret(v: Any) -> OValue:
        """Wrap a Python value as its canonical OValue.

        If used as the last expression of a Python block, this fixes the
        expression's OValue explicitly.
        """
        if isinstance(v, (ONull, OBool, OInt, OFloat, OStr, OList, OMap, OBlob)):
            return v
        return from_python(v)

    # ---- Meta-programming bridge -----------------------------------------

    def eval(self, expr: Any) -> OValue:
        """Evaluate an OExpr (or an AST node) against the live EvalContext.

        The expr argument is typically produced by `quote^(...)_quote`.
        Because we re-enter the SAME EvalContext, any env bindings the
        quoted expression references (e.g. python[0] state) see the
        current values -- this is how `O^(python[0]^(x=1)_python[0]
        python^(O.eval(q))_python)_O` works.
        """
        if self._ctx is None:
            raise RuntimeError(
                "O.eval called without an EvalContext. This Python backend "
                "was invoked outside the normal evaluator pipeline."
            )
        # Deferred imports to avoid circular imports at module load.
        from ..evaluator import _eval_expression
        from ..parser import ExpressionNode

        if isinstance(expr, OExpr):
            node = expr.ast
        elif isinstance(expr, ExpressionNode):
            node = expr
        else:
            raise TypeError(
                f"O.eval expected OExpr or ExpressionNode, got {type(expr).__name__}"
            )
        return _eval_expression(node, self._ctx)

    @staticmethod
    def quote(src: str) -> OExpr:
        """Parse a .O source fragment and return it as an unevaluated OExpr.

        Handy for programmatic code generation: build up a .O source
        string in Python, quote it, then O.eval it. If `src` parses to
        more than one top-level element, we wrap them in a synthetic
        O-node (same convention as quote^).
        """
        from ..parser import ExpressionNode, parse

        doc = parse(src)
        if len(doc.body) == 1 and isinstance(doc.body[0], ExpressionNode):
            return OExpr(doc.body[0])
        synthetic = ExpressionNode(
            language="O",
            env_id=0,
            env_explicit=False,
            body=list(doc.body),
        )
        return OExpr(synthetic)


# ---------------------------------------------------------------------------
# Opportunistic native-type -> OValue lifting
# ---------------------------------------------------------------------------

def _lift_result(x: Any) -> OValue:
    """Convert a Python result to an OValue, recognizing common rich types."""
    if isinstance(x, (ONull, OBool, OInt, OFloat, OStr, OList, OMap, OBlob)):
        return x  # user already wrapped it

    # matplotlib.figure.Figure -> PNG blob
    try:
        import matplotlib.figure
        if isinstance(x, matplotlib.figure.Figure):
            buf = io.BytesIO()
            x.savefig(buf, format="png", bbox_inches="tight", dpi=120)
            return OBlob(buf.getvalue(), "image/png")
    except Exception:
        pass

    # PIL Image -> PNG blob
    try:
        from PIL import Image as _PILImage
        if isinstance(x, _PILImage.Image):
            buf = io.BytesIO()
            x.save(buf, format="PNG")
            return OBlob(buf.getvalue(), "image/png")
    except Exception:
        pass

    if isinstance(x, bytes):
        return OBlob(x, "application/octet-stream")

    return from_python(x)


# ---------------------------------------------------------------------------
# PythonBackend
# ---------------------------------------------------------------------------

class PythonBackend:
    name = "python"

    def make_env(self) -> Dict[str, Any]:
        env: Dict[str, Any] = {
            "__name__": "__O_python__",
            "__builtins__": __builtins__,
            # Placeholder; replaced per-call in eval_ast with a ctx-bound
            # instance so O.eval(...) can re-enter the evaluator.
            "O": _OHelpers(None),
        }
        return env

    def render_child(self, child_value: OValue) -> str:
        """Default rendering of a nested child when we DON'T have access to
        the env (legacy path via the default evaluator flow).

        When eval_ast takes control (the normal path), we splice via a
        stash variable instead, which preserves rich values (including
        OExpr) rather than degrading them to repr.
        """
        if isinstance(child_value, OExpr):
            # Best-effort textual marker -- actual splicing happens in eval_ast.
            return "None"
        return repr(to_python(child_value))

    # ------------------------------------------------------------------ #
    # Structural hook: take full control of children so we can:          #
    #   1. Bind the per-call O helper to the live EvalContext, and       #
    #   2. Splice child OValues (including OExpr) as live Python objects #
    #      via auto-generated stash names, instead of only repr literals.#
    # ------------------------------------------------------------------ #
    def eval_ast(self, node, ctx) -> OValue:
        # Deferred imports avoid circular imports at module load time.
        from ..evaluator import _eval_expression
        from ..parser import ExpressionNode, TextPart

        env = ctx.env_for("python", node.env_id)
        # Bind O to a helper that can re-enter the current evaluator.
        env["O"] = _OHelpers(ctx)

        # A scratch namespace for child-value stashes keeps the user's
        # env clean of our generated names.
        stash_prefix = "__O_child_"
        stash: Dict[str, Any] = {}
        stash_idx = 0
        buf: List[str] = []

        for child in node.body:
            if isinstance(child, TextPart):
                buf.append(child.text)
            elif isinstance(child, ExpressionNode):
                child_value = _eval_expression(child, ctx)
                if isinstance(child_value, OExpr):
                    # Splice OExpr as a live object so user code can do
                    # `O.eval( <spliced> )`.
                    name = f"{stash_prefix}{stash_idx}"
                    stash_idx += 1
                    stash[name] = child_value
                    buf.append(name)
                else:
                    # Everything else: splice as the string representation
                    # that Python can parse. We still stash the lifted
                    # Python value under a name, so rich types (e.g.
                    # OBlob data) survive if users want to retrieve them.
                    py_val = to_python(child_value)
                    # Use repr-splice as before -- this preserves the
                    # existing semantics (e.g. inline numbers).
                    buf.append(repr(py_val))
            else:
                raise TypeError(f"Unknown AST child node: {child!r}")

        body_str = "".join(buf)
        # Merge the stash into env BEFORE exec so `__O_child_N` resolves.
        env.update(stash)
        return self._exec(body_str, env)

    # ------------------------------------------------------------------ #
    # Legacy evaluate(): kept for completeness and for the default flow  #
    # case where someone registers PythonBackend without the ast hook.   #
    # ------------------------------------------------------------------ #
    def evaluate(self, body: str, env: Dict[str, Any], ctx=None) -> OValue:
        if ctx is not None:
            # Refresh the O helper so O.eval works even through this path.
            env["O"] = _OHelpers(ctx)
        return self._exec(body, env)

    def _exec(self, body: str, env: Dict[str, Any]) -> OValue:
        # Python bodies inside .O are frequently nested inside indented
        # document contexts (HTML, Markdown), so they arrive with a
        # uniform leading indent that Python's parser rejects. Strip the
        # common leading whitespace the way a textwrap.dedent would,
        # AND strip any surrounding blank-line padding.
        body = textwrap.dedent(body).strip("\n")

        # Treat body as a full Python module.
        try:
            tree = ast.parse(body, mode="exec")
        except SyntaxError as e:
            raise RuntimeError(f"Python syntax error in O expression: {e}") from e

        # If the last statement is a bare expression, capture its value.
        last_is_expr = bool(tree.body) and isinstance(tree.body[-1], ast.Expr)
        if last_is_expr:
            original = tree.body[-1]
            assign = ast.Assign(
                targets=[ast.Name(id="__O_last__", ctx=ast.Store())],
                value=original.value,  # type: ignore[attr-defined]
            )
            ast.copy_location(assign, original)
            ast.fix_missing_locations(assign)
            tree.body[-1] = assign
            env["__O_last__"] = None

        code = compile(tree, "<O-python>", "exec")

        # Capture stdout for the case where the block does `print(...)` with
        # no terminating expression -- we still want to surface that text.
        stdout_capture = io.StringIO()
        old_stdout = sys.stdout
        sys.stdout = stdout_capture
        try:
            exec(code, env)
        finally:
            sys.stdout = old_stdout

        if last_is_expr:
            result = env.pop("__O_last__", None)
            if result is not None:
                return _lift_result(result)

        printed = stdout_capture.getvalue()
        if printed:
            # Strip the single trailing newline print() adds; preserve any
            # intentional blank lines inside the output.
            if printed.endswith("\n"):
                printed = printed[:-1]
            return OStr(printed)

        return ONull()
