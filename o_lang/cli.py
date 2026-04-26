"""
CLI entry point: `python -m O-lang <file.O> [--as FORMAT] [-o OUT]`

Usage:
    python -m O_lang examples/hello.O
    python -m O_lang examples/literate_report.O --as html -o report.html
    python -m O_lang examples/computed_plot.O --as html > out.html

--as controls final rendering of the root OValue:
    text      : render_plain (default when root is a scalar)
    html      : emit as HTML (strings passthrough, blobs as data URLs)
    markdown  : emit as raw Markdown
    latex     : emit as raw LaTeX
    json      : the OValue's JSON repr (for debugging)

If --as is not given, the output format mirrors the ROOT expression's
language: an `html^(...)_html` root prints HTML, a `markdown^(...)_markdown`
root prints Markdown, etc.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Optional

from . import __version__
from .backends import default_registry
from .evaluator import EvalContext, evaluate_document
from .ovalue import OList, OValue, render_plain, to_json_str
from .parser import Document, ExpressionNode, parse, pretty


def _root_expr(doc: Document) -> Optional[ExpressionNode]:
    """Return the single top-level ExpressionNode if the document has one.

    Ignores whitespace-only TextParts at the top level so that trailing
    newlines (common in source files) don't prevent root detection.
    """
    from .parser import TextPart  # local import to avoid widening the top imports

    meaningful = [
        c for c in doc.body
        if not (isinstance(c, TextPart) and not c.text.strip())
    ]
    if len(meaningful) == 1 and isinstance(meaningful[0], ExpressionNode):
        return meaningful[0]
    return None


def _root_language(doc: Document) -> Optional[str]:
    r = _root_expr(doc)
    return r.canonical_language if r else None


def _first_meaningful_child_language(doc: Document) -> Optional[str]:
    """Look inside an O^ root for its first substantive ExpressionNode's language.

    Lee's convention is that every .O script is wrapped in O^(...)_O, so the
    'effective' output language is usually the FIRST inner expression's.
    E.g. `O^(html^(...)_html)_O` should render as html by default.
    """
    r = _root_expr(doc)
    if r is None or r.canonical_language != "O":
        return None
    for child in r.body:
        if isinstance(child, ExpressionNode):
            return child.canonical_language
    return None


def _target_backend(fmt: str):
    """Return a backend instance for rendering into the target format."""
    from .backends import HtmlBackend, MarkdownBackend, LatexBackend, TextBackend
    backends = {
        "html": HtmlBackend(),
        "markdown": MarkdownBackend(),
        "latex": LatexBackend(),
        "text": TextBackend(),
    }
    backend = backends.get(fmt)
    if backend is None:
        raise SystemExit(f"Unknown --as format: {fmt!r}")
    return backend


def _render_final(doc: Document, value: OValue, fmt: str) -> str:
    """Render the evaluated root value into the target format.

    Special case: if the root expression was `O^(...)_O` and the result is
    an OList, we render each item independently via the target backend's
    render_child and concatenate. This reflects the sequencing semantics
    of O^ -- each child is a self-contained value in the target language,
    not a list-literal element.
    """
    if fmt == "json":
        return to_json_str(value)

    backend = _target_backend(fmt)

    root = _root_expr(doc)
    is_o_root = root is not None and root.canonical_language == "O"

    if is_o_root and isinstance(value, OList):
        # Sequence semantics: render each item in the target language
        # and join with a newline (a widely-acceptable separator for
        # html/markdown/latex/text alike).
        parts = [backend.render_child(item) for item in value.items]
        return "\n".join(parts)

    return backend.render_child(value)


def main(argv: Optional[list] = None) -> int:
    p = argparse.ArgumentParser(
        prog="O-lang",
        description=(
            "Run a .O file: polyglot, homoiconic, type-directed operational "
            "semantics. Document and code collapsed into one expression tree."
        ),
    )
    p.add_argument("file", help="path to a .O source file")
    p.add_argument(
        "--as", dest="fmt",
        choices=["auto", "text", "html", "markdown", "latex", "json"],
        default="auto",
        help="output format (default: mirror root expression language)",
    )
    p.add_argument(
        "-o", "--output", default=None,
        help="write output to file (default: stdout)",
    )
    p.add_argument(
        "--dump-ast", action="store_true",
        help="print parsed AST instead of evaluating",
    )
    p.add_argument(
        "-V", "--version", action="version",
        version=f"O-lang {__version__}",
    )
    args = p.parse_args(argv)

    src = Path(args.file).read_text(encoding="utf-8")
    doc = parse(src)

    if args.dump_ast:
        print(pretty(doc))
        return 0

    ctx = EvalContext()
    value = evaluate_document(doc, ctx)

    # Decide final rendering format.
    fmt = args.fmt
    if fmt == "auto":
        root_lang = _root_language(doc)
        if root_lang in ("html", "markdown", "latex", "text"):
            fmt = root_lang
        elif root_lang == "O":
            # An O^ wrapper -- inherit the first inner expression's language.
            inner = _first_meaningful_child_language(doc)
            fmt = inner if inner in ("html", "markdown", "latex", "text") else "text"
        else:
            fmt = "text"

    rendered = _render_final(doc, value, fmt)

    if args.output:
        Path(args.output).write_text(rendered, encoding="utf-8")
    else:
        sys.stdout.write(rendered)
        if not rendered.endswith("\n"):
            sys.stdout.write("\n")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
