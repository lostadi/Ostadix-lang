# O-lang

A polyglot, homoiconic meta-language where **every expression carries its own
interpreter as part of its syntax**. Born from the insight that evaluator
choice should be a structural property of the expression, not a global
setting.

```
o_root^(
  <p>The answer is python^(sum(x*x for x in range(10)))_python</p>
)_o_root
```

The `python^( ... )_python` is not a string, not a template, not a code
fence — it's an _expression_ whose parenthesis _type_ tells the runtime
which evaluator to use. Lisp made code and data share syntactic form. O
generalizes this across languages: every sub-expression declares its own
language, evaluates in its own persistent environment, and returns a
canonical `OValue` that any other language can consume.

---

## Quickstart

```bash
# Run a .O file
python -m o_lang examples/hello.O

# Render to HTML
python -m o_lang examples/computed_plot.O -o report.html

# Inspect the parse tree
python -m o_lang examples/literate_report.O --dump-ast

# Dump the raw OValue as JSON
python -m o_lang examples/hello.O --as json
```

The included examples:

| File | What it shows |
|------|---------------|
| `examples/hello.O` | HTML root with inline Python arithmetic. |
| `examples/computed_plot.O` | Python generates a matplotlib figure; HTML embeds it as a base64 data URL automatically. Separate `python[0]` / `python[1]` environments. |
| `examples/literate_report.O` | A Markdown document with Python computations woven through the prose. The document/code collapse in action. |

---

## The three moves that make this work

**1. Typed parentheses.** `LANG^( ... )_LANG` (or `LANG[n]^( ... )_LANG[n]`
for explicit environment selection). The opener's identifier is a
registered-language tag; the parser scans for the matching closer and
recursively parses any sub-expressions inside.

**2. OValue as the canonical intermediate.** Every expression evaluates to
an `OValue` — a tagged union of primitives, collections, blobs with mime
types, and unevaluated O expressions (for meta-level homoiconicity). Values
pass between languages by serializing through this single type.

**3. `render_child` per backend.** When a Python expression's value needs to
appear inside an HTML expression's body, the HTML backend's `render_child`
method decides how to embed it. An `OBlob(png_bytes, "image/png")` becomes
`<img src="data:image/png;base64,…">`. An `OList` becomes `<ul><li>…</li></ul>`.
The receiving language owns the rendering convention — which is how n
languages interoperate with O(n) code instead of O(n²).

---

## The deeper pattern

This is a runtime implementation of what the Transcompiler Composite
Framework's T3 theorem predicts theoretically: any lossless polyglot system
must route inter-language data through a canonical intermediate form. In
O-lang that canonical form is `OValue`, and it's made visible to the
programmer rather than hidden.

The `OExpr` constructor on `OValue` — which lets a value carry an
unevaluated O AST — is what lifts the system past "polyglot notebook" into
"programmable metalanguage." An O program can produce O code as a value and
evaluate it. The same meta-circular property that gives Lisp its power,
generalized across a multi-language universe.

See `SPEC.md` for the formal language specification.

---

## Architecture

```
o_lang/
├── ovalue.py              # OValue tagged union (the L* fiber)
├── parser.py              # Typed-paren parser (context-free wrt inner langs)
├── evaluator.py           # Leaves-up tree evaluator + env registry
├── cli.py                 # python -m o_lang entry
├── __main__.py
└── backends/
    ├── base.py            # Backend protocol
    ├── python_backend.py  # Real exec, persistent globals, rich-value lifting
    ├── html_backend.py    # Passthrough + data-URL embedding
    ├── markdown_backend.py
    ├── latex_backend.py
    └── text_backend.py
```

Adding a new language takes three steps: write a `Backend` with
`make_env`, `render_child`, `evaluate`; register it in
`backends/__init__.py::default_registry`; and add its tag to
`REGISTERED_LANGUAGES` in `parser.py`. The core parser does not change.

---

## Status

v0.1.0 — MVP. Five backends, persistent envs per `[n]`, leaves-up eager
evaluation, opportunistic rich-value lifting for matplotlib/PIL.

Not yet: named bindings (`$var`), lazy/reactive evaluation, a macro system,
dynamic dispatch on evaluator tag. See `SPEC.md § 7`.

---

## License

This is research scaffolding for the .O idea. Use it, extend it, break it.
