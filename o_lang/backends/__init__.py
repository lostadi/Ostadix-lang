"""Registry of language backends. Add new backends here."""

from typing import Dict

from .base import Backend
from .python_backend import PythonBackend
from .markdown_backend import MarkdownBackend
from .html_backend import HtmlBackend
from .latex_backend import LatexBackend
from .text_backend import TextBackend
from .o_backend import OBackend
from .quote_backend import QuoteBackend
from .nix_backend import NixBackend
from .nix_store_backend import NixStoreBackend
from .nixos_test_backend import NixOSTestBackend


def default_registry() -> Dict[str, Backend]:
    """Return a fresh registry mapping canonical language tags -> Backend."""
    return {
        "python": PythonBackend(),
        "markdown": MarkdownBackend(),
        "html": HtmlBackend(),
        "latex": LatexBackend(),
        "text": TextBackend(),
        # O^ is the host/sequencing backend -- it evaluates children in source
        # order and returns a single value (or an OList if several). This is
        # the canonical wrapper for full .O scripts.
        "O": OBackend(),
        # quote^ captures its body as an OExpr without evaluating. Companion
        # to O.eval() inside Python blocks -- the two together give O its
        # Lisp-style homoiconicity.
        "quote": QuoteBackend(),
        # Nix backends: typed evaluator, store-path realizer, OS test runner.
        "nix": NixBackend(),
        "nix_store": NixStoreBackend(),
        "nixos_test": NixOSTestBackend(),
    }


__all__ = [
    "Backend",
    "PythonBackend",
    "MarkdownBackend",
    "HtmlBackend",
    "LatexBackend",
    "TextBackend",
    "OBackend",
    "QuoteBackend",
    "NixBackend",
    "NixStoreBackend",
    "NixOSTestBackend",
    "default_registry",
]
