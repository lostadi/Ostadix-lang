# O-lang (Rust edition)

A polyglot, homoiconic meta-language where **every expression carries its own
interpreter as part of its syntax**. Born from the insight that evaluator
choice should be a structural property of the expression, not a global
setting.

```
html^(
  <p>The answer is python^(
__oval_result__ = sum(x*x for x in range(10))
)_python</p>
)_html
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
# Build the runtime
cargo build

# Run a .O file (shim_dir defaults to backends/)
cargo run -- examples/hello.O
cargo run -- examples/hello.O backends

# Compile a .O file into a self-contained native binary
cargo run --bin olangc -- examples/hello.O -o hello
./hello

# Python reference implementation (used by Python test suite)
python -m o_lang examples/hello.O
python -m o_lang examples/hello.O --dump-ast
python -m o_lang examples/hello.O --as json
```

### `let` bindings

Top-level `let` bindings assign the result of an expression to a `$name`
that can be spliced into subsequent expressions:

```
let answer = python^(
__oval_result__ = 40 + 2
)_python

python^(
__oval_result__ = $answer + 1
)_python
```

### Included examples

| File | What it shows |
|------|---------------|
| `examples/hello.O` | Minimal Python arithmetic. |
| `examples/bindings.O` | `let` binding and `$var` splice. |
| `examples/nested_splice.O` | A Python block nested inside another Python block. |
| `examples/html_python_html.O` | HTML root with inner Python that itself generates HTML. |
| `examples/html_escape.O` | HTML-escaping of spliced values. |
| `examples/html_raw_roundtrip.O` | Passthrough of raw HTML fragments via `OHtml`. |
| `examples/nix_basic.O` | Nix expressions evaluated inside O. |
| `examples/nix_python_html.O` | Nix → Python → HTML value pipeline. |
| `examples/nix_storepath.O` | Nix-derived store paths rendered as HTML links. |
| `examples/nix_storepath_python.O` | Python reading a Nix `OStorePath`. |
| `examples/persist.O` | Persistent per-`[n]` Python environments across expressions. |
| `examples/env_split.O` | Two independent Python environments in one document. |
| `examples/ephemeral.O` | Ephemeral (single-use) environments via `env_id = u32::MAX`. |
| `examples/meta_eval.O` | `quote^` and `O.eval` — homoiconicity across languages. |

---

## The three moves that make this work

**1. Typed parentheses.** `LANG^( ... )_LANG` (or `LANG[n]^( ... )_LANG[n]`
for explicit environment selection). The opener's identifier is a
registered-language tag; the parser scans for the matching closer and
recursively parses any sub-expressions inside.

**2. OValue as the canonical intermediate.** Every expression evaluates to
an `OValue` — a tagged union of primitives, collections, blobs with MIME
types, store paths, and raw HTML. Values pass between languages by
serializing through this single type over newline-delimited JSON.

**3. `render_child` per backend.** When a Python expression's value needs to
appear inside an HTML expression's body, the HTML backend's `render_child`
decides how to embed it. An `OBlob(png_bytes, "image/png")` becomes
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
evaluate it. The same meta-circular property that gives Lisp's `quote`/`eval`,
generalized across a multi-language universe.

See `SPEC.md` for the formal language specification.

---

## Architecture

This repo contains two implementations:

### Rust runtime (`src/`) — primary binary

```
src/
├── value.rs      # OValue sum type + JSON wire protocol. Pure data layer.
├── parser.rs     # Typed-paren parser → ONode tree
├── eval.rs       # Applicative-order leaves-up evaluator + render_child dispatch
├── process.rs    # ProcessRegistry: one subprocess per (lang, env_id) key
├── nix_ops.rs    # Inline Nix expression evaluation
├── nixos_ops.rs  # NixOS test driver integration
└── bin/
    └── olangc.rs # AOT compiler: .O source → self-contained native binary
```

Backend shims (`backends/`) are subprocess scripts. The Rust runtime
communicates with them over **newline-delimited JSON IPC**:

- Runtime → shim: `{"cmd":"exec","env_id":0,"body":"...","scope":{...}}`
- Shim → runtime: `{"status":"ok","value":{"t":"int","v":42}}`
  or `{"status":"err","message":"..."}`

Shim resolution order for language tag `<lang>` under `shim_dir`:
`<lang>_shim.py` → `<lang>_shim` → `<lang>.py` → `<lang>`

**Exception:** `html` is handled entirely inline in `eval.rs` — no subprocess.

### Python reference implementation (`o_lang/`) — used by Python test suite

```
o_lang/
├── ovalue.py              # OValue tagged union
├── parser.py              # Typed-paren parser
├── evaluator.py           # Leaves-up tree evaluator + env registry
├── cli.py                 # python -m o_lang entry point
└── backends/
    ├── base.py
    ├── python_backend.py
    ├── html_backend.py
    ├── markdown_backend.py
    ├── latex_backend.py
    └── text_backend.py
```

### Registered backends

| Tag          | Shim / handler          | Notes |
|--------------|-------------------------|-------|
| `python`     | `python_shim.py`        | Real `exec`, persistent globals per env. Returns `__oval_result__` or captured stdout. |
| `html`       | inline (`eval.rs`)      | Source passthrough. Blobs → `data:` URL `<img>`. |
| `markdown`   | shim                    | Source passthrough. |
| `latex`      | shim                    | Source passthrough. |
| `bash`       | shim                    | Shell execution. |
| `shell`      | shim                    | Shell execution (alias). |
| `rust`       | shim                    | Rust snippet execution. |
| `racket`     | shim                    | Racket evaluation. |
| `nix`        | `nix_shim.py`           | Nix expression evaluation via `nix-instantiate`. |
| `nix_expr`   | inline (`nix_ops.rs`)   | Nix expression → derivation. |
| `nix_store`  | `nix_store_shim.py`     | Builds a derivation and returns an `OStorePath`. |
| `nixos_test` | `nixos_test_shim.py`    | NixOS VM test driver. |

Adding a new language (Rust runtime): write `backends/<lang>_shim.py`
implementing `exec` / `cleanup` / `ping`, add the tag to
`registered_backends` in `src/main.rs`, and add a `render_child` branch in
`eval.rs` if the language needs non-default value splicing.

---

## OValue wire format

Every value crossing a language boundary is an `OValue`. JSON encoding:

```json
{"t":"null"}
{"t":"bool","v":true}
{"t":"int","v":42}
{"t":"float","v":3.14}
{"t":"str","v":"hello"}
{"t":"html","v":"<p>...</p>"}
{"t":"store_path","path":"/nix/store/..."}
{"t":"list","v":[...]}
{"t":"map","v":{"key":{...}}}
{"t":"blob","v":"<base64>","mime":"image/png"}
```

---

## Running the tests

```bash
# Rust unit tests (inline in src/value.rs)
cargo test

# Python reference impl tests
python -m tests.test_parser
python -m tests.test_evaluator

# Integration smoke tests (cargo run + grep)
./test_o_lang_examples.sh
```

---

## `olangc` — AOT compiler

`olangc` compiles a `.O` file into a self-contained native binary. The
binary embeds the program source, all backend shim scripts, and the O-lang
runtime. It still requires the language runtimes used by the program (Python,
Nix, etc.) to be installed on the target machine.

```bash
# Build olangc first
cargo build --bin olangc

# Compile a .O program
olangc examples/hello.O          # output: ./hello
olangc examples/hello.O -o mybin # explicit output name
olangc examples/hello.O --shim-dir ./backends --keep-build-dir
```

---

## Status

v0.1.0 — Rust runtime primary, Python reference implementation for
cross-validation. Twelve registered backends, persistent envs per `[n]`,
leaves-up eager evaluation, `let` bindings, `lazy^` / `defer^` policy
blocks, `quote^` + `O.eval` homoiconicity, `olangc` AOT compiler.

See `SPEC.md` for the formal language specification and known limitations.

---

## License

This is research scaffolding for the .O idea. Use it, extend it, break it.
