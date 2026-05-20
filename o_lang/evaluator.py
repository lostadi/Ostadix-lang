"""
Evaluator: walks the AST produced by parser.py and evaluates each
ExpressionNode via its language backend, threading a registry of persistent
per-(language, env_id) environments.

Evaluation order: LEAVES UP (applicative order, like standard Lisp).
A node's children are evaluated first; their OValues are then rendered
into the parent's language via the parent backend's render_child(); the
parent backend then runs its evaluate() on the fully-spliced body string.

Persistent environments: every unique (canonical_language, env_id) pair
gets its own env object created exactly once via backend.make_env() and
reused for every expression that references it. This gives you a REPL-
like shell per bracket-labeled env, surviving across evaluations.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Dict, List, Tuple, Union

from .backends import default_registry
from .backends.base import Backend
from .ovalue import OStr, OValue
from .parser import Document, ExpressionNode, LetBinding, TextPart, VarRef


# (canonical_language, env_id) -> persistent env object
EnvRegistry = Dict[Tuple[str, int], object]


@dataclass
class EvalContext:
    backends: Dict[str, Backend] = field(default_factory=default_registry)
    envs: EnvRegistry = field(default_factory=dict)

    def backend_for(self, canonical_language: str) -> Backend:
        if canonical_language not in self.backends:
            raise KeyError(
                f"No backend registered for language {canonical_language!r}. "
                f"Known: {sorted(self.backends)}"
            )
        return self.backends[canonical_language]

    def env_for(self, canonical_language: str, env_id: int) -> object:
        key = (canonical_language, env_id)
        if key not in self.envs:
            self.envs[key] = self.backend_for(canonical_language).make_env()
        return self.envs[key]


def evaluate_document(doc: Document, ctx: EvalContext = None) -> OValue:
    """Evaluate a parsed .O document and return the root OValue.

    If the document contains a single top-level expression, its OValue is
    the result. If it contains multiple top-level expressions (or mixed
    text/expressions), they are concatenated as a Text root -- we synthesize
    an implicit text[0] wrapper so that every document has a single root.
    """
    ctx = ctx or EvalContext()
    scope: Dict[str, OValue] = {}

    top_body = doc.body

    # Ignore whitespace-only TextParts at the top level so a trailing
    # newline (ubiquitous in source files) doesn't prevent us from
    # treating a single expression as the root.
    meaningful = [
        c for c in top_body
        if not (isinstance(c, TextPart) and not c.text.strip())
    ]

    # Evaluate let bindings first (they may appear before the main expr).
    # Collect non-let nodes for the synthesized root.
    non_let = []
    for node in top_body:
        if isinstance(node, LetBinding):
            # Temporarily stash scope in ctx so backends (like eval_ast) can
            # access it during recursive evaluation.
            ctx._scope = scope
            val = _eval_expression(node.expr, ctx, scope)
            scope[node.name] = val
        else:
            non_let.append(node)

    # Ensure scope is available on ctx throughout the rest of evaluation.
    ctx._scope = scope

    meaningful_non_let = [
        c for c in non_let
        if not (isinstance(c, TextPart) and not c.text.strip())
    ]

    # If the document is exactly one ExpressionNode (ignoring stray
    # whitespace and let bindings), that expression IS the root.
    if len(meaningful_non_let) == 1 and isinstance(meaningful_non_let[0], ExpressionNode):
        return _eval_expression(meaningful_non_let[0], ctx, scope)

    # Otherwise synthesize an implicit text root so we always return a single
    # OValue. The text backend renders children using render_plain.
    synthetic_root = ExpressionNode(
        language="text",
        env_id=0,
        env_explicit=False,
        body=non_let,
    )
    return _eval_expression(synthetic_root, ctx, scope)


def _eval_expression(
    node: ExpressionNode,
    ctx: EvalContext,
    scope: Dict[str, OValue] = None,
) -> OValue:
    """Evaluate one ExpressionNode.

    If the backend implements `eval_ast(node, ctx)`, the backend takes
    full control of child evaluation (used by O^, which sequences
    children, and quote^, which captures the AST without evaluating).

    Otherwise we use the default flow:
      1. Evaluate every child ExpressionNode recursively.
      2. Build the final body string by concatenating TextParts verbatim
         with child render_child() results.
      3. Call the backend's evaluate() with the spliced body.
    """
    if scope is None:
        scope = {}

    backend = ctx.backend_for(node.canonical_language)

    eval_ast = getattr(backend, "eval_ast", None)
    if callable(eval_ast):
        return eval_ast(node, ctx)

    env = ctx.env_for(node.canonical_language, node.env_id)

    buf: List[str] = []
    for child in node.body:
        if isinstance(child, TextPart):
            buf.append(child.text)
        elif isinstance(child, VarRef):
            # Substitute variable reference from scope.
            val = scope.get(child.name)
            if val is None:
                # Emit the raw $name text so it reaches the backend
                # (might be a Python variable already in the backend env).
                buf.append(f"${child.name}")
            else:
                buf.append(backend.render_child(val))
        elif isinstance(child, ExpressionNode):
            child_value = _eval_expression(child, ctx, scope)
            buf.append(backend.render_child(child_value))
        else:
            raise TypeError(f"Unknown AST child node: {child!r}")

    body_str = "".join(buf)
    # Pass ctx to evaluate when the backend accepts it (Python uses this
    # for O.eval; others don't care).
    try:
        return backend.evaluate(body_str, env, ctx)
    except TypeError:
        # Backward compat with backends that still have 2-arg evaluate.
        return backend.evaluate(body_str, env)


# ---------------------------------------------------------------------------
# Convenience top-level driver
# ---------------------------------------------------------------------------

def run(src: str, ctx: EvalContext = None) -> OValue:
    """Parse + evaluate a .O source string. Returns the root OValue."""
    from .parser import parse
    return evaluate_document(parse(src), ctx)