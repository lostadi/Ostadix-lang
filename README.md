# O-lang (Rust edition)

> **Every expression carries its own interpreter as part of its syntax.**

O-lang is a meta-language built on one radical idea: the language an expression
is written in is a structural part of the expression itself — not a file
extension, not a global mode switch, not a pragma. You write the language name
directly around the code, and the runtime dispatches to that language's
evaluator on the spot.

```
html^(
  <p>The answer is python^(
__oval_result__ = sum(x*x for x in range(10))
)_python.</p>
)_html
```

The `python^( ... )_python` block is not a string, not a template, not a
code fence. It is an *expression*. Its parenthesis shape — `LANG^(` ... `)_LANG`
— is the syntax that says "evaluate this in Python." The result is a value
that HTML can embed directly, without either side knowing about the other's
type system.

---

## Table of Contents

1. [What is new here?](#what-is-new-here) — the novel ideas, explained plainly
2. [Gentle introduction](#gentle-introduction) — for readers new to programming languages
3. [Quickstart](#quickstart) — build and run in three commands
4. [Language tour](#language-tour) — all features with examples
5. [Architecture](#architecture) — how the runtime works
6. [Reference](#reference) — wire format, backends, builtins
7. [Running the tests](#running-the-tests)
8. [Status and roadmap](#status)

---

## What is new here?

Most languages make one or all of these assumptions:

* A program is written in one language.
* When you call out to another language you use an FFI (foreign-function
  interface) — a bridge bolted on the side.
* The "language" a piece of code belongs to is determined by the file it sits
  in, or by a special import/escape mechanism.

O-lang breaks all three assumptions. Here are the four ideas that make it
different.

---

### 1. Typed parentheses: the language is in the syntax

In every language you have ever used, parentheses are anonymous. `(x + y)`
is just grouping; nothing about the parentheses tells you what evaluator will
handle the contents.

O-lang gives parentheses a *type*: the identifier before `^(` names the
evaluator, and the matching `)_IDENT` closes it.

```
python^( 6 * 7 )_python          # evaluated by the Python runtime → 42
html^( <b>hello</b> )_html       # rendered as an HTML fragment
markdown^( **bold** )_markdown   # rendered as Markdown
nix^( builtins.nixVersion )_nix  # evaluated by the Nix expression language
```

These are not escape sequences inside another language. They are first-class
expressions in their own right, and they nest freely:

```
html^(
  <p>Count: python^( sum(range(10)) )_python</p>
)_html
```

The Python expression is evaluated first (inner-to-outer), its result
is converted to something HTML can embed, and then the HTML expression
completes. **No FFI. No bridge. The nesting is the interface.**

---

### 2. OValue: the universal exchange type

When Python produces `42` and HTML needs to embed it, *something* has to
cross the boundary. In O-lang that something is always an `OValue` — a
tagged union that every backend speaks.

```
ONull | OBool | OInt | OFloat | OStr
OHtml | OList | OMap | OBlob(bytes, mime_type)
OStorePath | OExpr | ONixExpr | ODerivation | OSystem
```

The critical insight is that **the receiving language decides how to render
a foreign value, not the sending language**. This is the `render_child`
method: each backend knows how to turn any `OValue` into its own source
syntax.

```
HTML.render_child(OBlob(png, "image/png"))
  → <img src="data:image/png;base64,…">

HTML.render_child(OList([OStr("a"), OStr("b")]))
  → <ul><li>a</li><li>b</li></ul>

Python.render_child(OInt(42))
  → 42  (a Python literal)
```

With N languages and this single protocol, interoperability costs O(N) code
— one `render_child` per language — instead of O(N²) bridges between
every pair.

This is a concrete implementation of what formal polyglot theory (the
Transcompiler Composite Framework's T3 theorem) predicts must exist in any
*lossless* polyglot system: a canonical intermediate representation that all
parties route through. In O-lang it is visible to the programmer rather than
hidden in a compiler.

---

### 3. Persistent, named environments

In a Jupyter notebook, all Python cells share one implicit global namespace.
In O-lang, environments are explicit and multiple:

```
python[0]^( x = 10 )_python[0]   # sets x in environment 0
python[1]^( x = 99 )_python[1]   # sets x in a completely separate environment 1
python[0]^( x * x  )_python[0]   # sees 10, not 99 → returns 100
```

The number in brackets is an *environment index*. Environments survive for
the life of the document. State — imports, function definitions, variable
bindings — persists across all expressions that reference the same
`(language, index)` pair.

`python^(...)_python` without brackets defaults to `python[0]`. You can have
as many independent Python (or Nix, or Racket, …) environments as you need,
all live at once in the same document.

---

### 4. Homoiconicity across languages

Lisp is famous for *homoiconicity*: in Lisp, code and data have the same
shape, so a program can treat another program as data, transform it, and
evaluate it. This is how Lisp macros work, and it is a deep source of
expressive power.

O-lang generalizes homoiconicity across multiple languages. The `quote^`
backend captures any expression — in any language — as an `OExpr` value
without evaluating it:

```
let q = quote^( python^( 6 * 7 )_python )_quote
# q is now a value of type OExpr — an unevaluated AST
```

A Python block can receive `q`, inspect it, store it in a list, and evaluate
it on demand with `O.eval(q)`:

```
python[0]^(
  result = O.eval(q)   # fires the Python evaluator on the quoted expression → 42
)_python[0]
```

Python code can also *construct* O source programmatically and quote it:

```
python[0]^(
  src = "python^(2 ** 10)_python"
  O.eval(O.quote(src))            # → 1024
)_python[0]
```

The full quote/eval round-trip works across all registered backends. A
program can build up an O expression tree in Python, pass it through a
Nix backend, and evaluate the result — the language boundary is not a
barrier to metaprogramming.

---

## Gentle introduction

*This section is for readers who are new to the idea of programming
languages as objects of study. You do not need prior experience with
compilers, interpreters, or language theory. You need only curiosity.*

---

### What is a programming language, really?

When you write `2 + 2` in Python and run it, something has to *interpret*
those characters and produce the number `4`. That something is an evaluator
(also called an interpreter or runtime). Every programming language is, at
bottom, a pair of things:

1. **Syntax** — the rules about what text is a valid program.
2. **Semantics** — the rules about what a valid program *does*.

The evaluator reads syntax and applies semantics to produce a result.

Most of the time, you pick a language and then write your whole program in it.
The evaluator is fixed for the whole file.

---

### The problem O-lang is solving

Imagine you are writing a web page. The page is HTML. But the page needs
a number computed in Python. And the number comes from a database query
written in SQL. And the SQL is generated from a Nix configuration that
manages your database server.

Today you would write four separate programs in four separate files, wire
them together with shell scripts, and spend a lot of time making sure the
data types line up at each boundary. The language boundaries are friction.

O-lang's answer is: **let the expression carry its evaluator**. Instead of
choosing one language for the file, write each piece of the program in the
language that fits it, right where it belongs, nested inside the other
pieces.

---

### Your first O-lang program

The smallest possible program:

```
python^(
__oval_result__ = 1 + 1
)_python
```

Read it as: "evaluate this body in Python." The `python^(` opener and
`)_python` closer are a matched pair. Everything between them is Python code.
The special variable `__oval_result__` is how a Python block says "this is my
return value." (You can also just write a bare expression on the last line,
like a Python REPL.)

Run it:

```bash
cargo run -- examples/hello.O
```

Output: `2`

---

### Nested expressions

Now nest a Python block inside an HTML block:

```
html^(
  <h1>The answer is python^(
__oval_result__ = 6 * 7
)_python!</h1>
)_html
```

The evaluator works *inside-out* (leaves before roots, like arithmetic).
First the Python block runs and produces `42`. Then the HTML block sees
`42` where the Python block used to be. The HTML backend turns `42` into
the string `"42"` and embeds it, producing:

```html
<h1>The answer is 42!</h1>
```

No string interpolation library. No template engine. The nesting *is* the
template.

---

### Naming values with `let`

You can give a name to the result of any expression:

```
let answer = python^(
__oval_result__ = 40 + 2
)_python

python^(
__oval_result__ = $answer + 1
)_python
```

The `let answer = ...` line runs the Python block and binds its result to
`$answer`. The second block splices `$answer` in — the runtime substitutes
the value `42` before sending the body to Python, so Python sees
`__oval_result__ = 42 + 1` and returns `43`.

---

### Persistent state across blocks

One of the most practical features: state persists across blocks that share
the same environment index.

```
python[0]^(
import random
random.seed(42)
samples = [random.gauss(0, 1) for _ in range(500)]
)_python[0]

python[0]^(
# `samples` is still here from the block above
round(sum(samples) / len(samples), 4)
)_python[0]
```

This makes O-lang documents feel like literate programs or lab notebooks,
except that the environments are *explicit*, *named*, and *multiple* — you
can have several independent Python namespaces in one document with no
accidental sharing between them.

---

### The universal value type

Every O-lang expression returns an `OValue`. Think of `OValue` as the
common currency that all languages in a document share. When a Python
block returns a list, it becomes `OList`. When it returns a number, it
becomes `OInt`. When an HTML block needs to embed that value, it looks at
the type tag and decides how to render it.

The remarkable thing is that *you do not have to teach Python about HTML or
HTML about Python*. You only have to teach each language about `OValue`.
The complexity does not grow as you add languages.

---

### Code as data: `quote` and `O.eval`

One of the deepest ideas in programming language theory is
*homoiconicity* — the ability of a language to treat its own programs as
data that can be inspected and modified.

O-lang lets you do this not just within one language but across all of them.
The `quote^(...)_quote` expression captures its body as an `OExpr` value
(an unevaluated program fragment) rather than running it:

```
let q = quote^( python^(2 ** 10)_python )_quote
# q is just data now — nothing has run yet
```

Later, from inside Python, you can evaluate it:

```
python[0]^(
O.eval(q)    # → 1024
)_python[0]
```

You can also build O-lang source code programmatically in Python and then
evaluate it. This is the same power that makes Lisp famously expressive,
now available in a multi-language system.

---

### The Nix dimension

For readers familiar with NixOS or Nix package manager: O-lang treats the
operating system as a participant in the value model. A Nix expression is
an `OValue`. A derivation (a package build specification) is an `OValue`.
A built store path is an `OValue`. An active system configuration is an
`OValue`.

These values flow through the same typed-expression mechanism as everything
else:

```
let cfg  = nix_expr^( (import <nixpkgs/nixos> { configuration = ./conf.nix; }).system )_nix_expr
let drv  = instantiate($cfg)   # NixExpr → Derivation
let path = realise($drv)       # Derivation → StorePath
let sys  = activate($path)     # StorePath → System
```

Each step is a pure function from one `OValue` to another. The OS is not
a side effect of the program — it is a value the program can compute and
pass around.

---

## Quickstart

```bash
# Clone and build
git clone https://github.com/lostadi/O-lang_rust_edition
cd O-lang_rust_edition
cargo build

# Run a .O file
cargo run -- examples/hello.O
cargo run -- examples/literate_report.O --as markdown

# Compile to a self-contained native binary
cargo run --bin olangc -- examples/hello.O -o hello
./hello

# Python reference implementation (for cross-checking)
python -m o_lang examples/hello.O
python -m o_lang examples/hello.O --dump-ast
python -m o_lang examples/hello.O --as json
```

---

## Language tour

### Typed expression syntax

```
LANG^( body )_LANG
LANG[n]^( body )_LANG[n]    # explicit environment index n
```

The opener `LANG^(` and closer `)_LANG` must match exactly. `LANG` must be a
registered language tag (see the backend table below). Every non-language
identifier — including operators like `2^(x+1)` inside a Python block — is
left alone by the parser.

#### Aliases

| Alias   | Canonical  |
|---------|------------|
| `py`    | `python`   |
| `md`    | `markdown` |
| `tex`   | `latex`    |
| `plain` | `text`     |
| `o`     | `O`        |

#### Shebang support

`.O` files can start with `#!/usr/bin/env O` — the runtime strips it before
parsing.

---

### `let` bindings and `$var` splicing

```
let name = LANG^( ... )_LANG
```

Runs the expression and binds its `OValue` result to `$name`. Any subsequent
expression that contains `$name` in its body has the value substituted before
the body is sent to the backend.

```
let answer = python^( __oval_result__ = 40 + 2 )_python

html^( <p>The answer is $answer.</p> )_html
```

---

### Result from a Python block

In a Python block the return value is, in order of priority:

1. `__oval_result__` — set explicitly by the block.
2. The value of the last bare expression (no assignment), matching Python REPL
   semantics.
3. Captured `stdout` — if neither of the above apply, anything written to
   `print()` is returned as `OStr`.

```
python^( 6 * 7 )_python             # → OInt(42), trailing-expression form
python^( print("hi") )_python       # → OStr("hi\n"), stdout capture
python^( __oval_result__ = 99 )_python  # → OInt(99), explicit form
```

---

### `O^(...)_O` sequencing block

The `O` backend is the document host. It evaluates its children
left-to-right and collects their results. One non-null child → returns
it directly. Multiple non-null children → returns them as `OList`. All
null → returns `ONull`.

```
O^(
  python[0]^( x = 10 )_python[0]
  python[0]^( x * x  )_python[0]
)_O
```

This is the canonical outer wrapper for full `.O` documents.

---

### Persistent environments

```
python[0]^( x = 40 )_python[0]
python[0]^( x + 2  )_python[0]   # → 42, x is still in scope
python[1]^( x      )_python[1]   # NameError: x not defined in env 1
```

Each unique `(language, index)` pair has its own isolated subprocess. State
inside it survives until the process exits. There is no accidental sharing
between different indices.

---

### Lazy and deferred blocks

Append `{lazy}` or `{defer}` to a language tag to capture the block as a
thunk without immediately running the backend:

```
let thunk = python{defer}^( import time; time.time() )_python{defer}
# No Python subprocess has run yet. thunk is a Request[Eval] value.
let result = now($thunk)   # force it here → current timestamp
```

- **`{lazy}`** — pure backends only; the result is cached by content
  fingerprint. Evaluating the same expression twice returns the cached
  value.
- **`{defer}`** — any backend; never cached; re-runs every time `now()` is
  called on it.

The counterpart `lazy(expr)` builtin wraps any expression in a
`Policy::Lazy` context without block-level syntax, and `now(req)` forces
any `Request` value.

---

### Homoiconicity: `quote^` and `O.eval`

```
O^(
  python[0]^(
    q = quote^( python^(6 * 7)_python )_quote   # q : OExpr
  )_python[0]
  python[0]^(
    O.eval(q)                                    # → 42
  )_python[0]
)_O
```

`quote^(...)_quote` captures its body as an unevaluated `OExpr` value —
nothing runs. `O.eval(expr)` re-enters the evaluator on a live `OExpr`,
using the current persistent environments. Python code can also call
`O.quote(src_string)` to parse a raw source fragment and return it as
`OExpr`, enabling fully programmatic construction of O expressions.

---

### The Nix lattice (four-rung pipeline)

O-lang models the full Nix build pipeline as a typed value chain:

```
nix_expr^(...)_nix_expr   →   ONixExpr
instantiate($expr)         →   ODerivation
realise($drv)              →   OStorePath
activate($path)            →   OSystem
```

Each builtin takes an `OValue` of the expected type and returns the next
rung. `current_system()` returns an `OSystem` for the currently active
profile without performing any transition.

`activate()` defaults to dry-run unless `O_LANG_ALLOW_ACTIVATION=1` is set.

---

### Autonomous scheduling

```
autonomous(
  O^(
    let a = nix_expr^( ... )_nix_expr
    let b = nix_expr^( ... )_nix_expr
    ...
  )_O
)
```

Inside `autonomous(...)`, Nix-family Requests (instantiate, realise) are
buffered rather than executed eagerly. When a *force point* is reached, the
`AutonomousScheduler` flushes the buffer by executing all pending Requests
concurrently in topological order. Eval requests (Python, bash, etc.) still
execute eagerly.

---

### Builtin call reference

| Call | Input → Output | Description |
|------|---------------|-------------|
| `instantiate(expr)` | `ONixExpr` → `ODerivation` | Runs `nix-instantiate`. |
| `realise(drv)` | `ODerivation` → `OStorePath` | Builds the derivation. |
| `activate(path)` | `OStorePath` → `OSystem` | Switches system profile (dry-run by default). |
| `current_system()` | — → `OSystem` | Returns the currently active profile. |
| `lazy(expr)` | any → `ORequest` | Wraps in `Policy::Lazy`; deferred until forced. |
| `now(req)` | `ORequest` → `OValue` | Forces a deferred Request. |
| `autonomous(expr)` | any → `OValue` | Buffers Nix Requests; flushes concurrently at force points. |

---

### Included examples

| File | What it demonstrates |
|------|----------------------|
| `examples/hello.O` | Minimal Python arithmetic — the smallest runnable program. |
| `examples/bindings.O` | `let` binding and `$var` splice. |
| `examples/nested_splice.O` | A Python block nested inside another Python block. |
| `examples/html_basic.O` | HTML template with an embedded Python computation. |
| `examples/html_python_html.O` | HTML root with inner Python that itself generates HTML. |
| `examples/python_html_python.O` | Python outer ▶ HTML inner ▶ Python innermost — three languages, two boundaries. |
| `examples/html_escape.O` | HTML-escaping of spliced values. |
| `examples/html_raw_roundtrip.O` | Passthrough of raw HTML fragments via `OHtml`. |
| `examples/computed_plot.O` | Matplotlib figure returned as `OBlob` and rendered as `<img>` by HTML. |
| `examples/literate_report.O` | Literate report: Markdown wrapping persistent Python environments. |
| `examples/persist.O` | Persistent per-`[n]` Python environments across expressions. |
| `examples/env_split.O` | Two independent Python environments in one document. |
| `examples/ephemeral.O` | Ephemeral (single-use) environments. |
| `examples/trailing_expr.O` | Trailing expression returns value without `__oval_result__`. |
| `examples/meta_eval.O` | `quote^` and `O.eval` — homoiconicity across languages. |
| `examples/nix_basic.O` | Nix expressions evaluated inside O. |
| `examples/nix_python_html.O` | Nix → Python → HTML value pipeline. |
| `examples/nix_storepath.O` | Nix-derived store paths rendered as HTML links. |
| `examples/nix_storepath_python.O` | Python reading a Nix `OStorePath`. |
| `examples/instantiate_realise_basic.O` | `nix_expr^`, `instantiate()`, and `realise()` rung climb. |
| `examples/lazy_request_basic.O` | `lazy(...)` — constructs `Request` values without executing them. |
| `examples/lazy_defer_attrs_basic.O` | `{lazy}` and `{defer}` block attributes — thunk creation. |
| `examples/os_as_participant_basic.O` | OS-as-participant: `activate()`, `current_system()`, the four-rung Nix lattice. |
| `examples/nixos_test.O` | Single-machine NixOS VM test inside an O-lang script. |
| `examples/nixos_test_two_machine.O` | Two-machine NixOS test (server + client). |

---

## Architecture

This repo ships two implementations of the same spec. The Rust runtime is
authoritative.

### Rust runtime (`src/`) — primary binary

```
src/
├── value.rs      # OValue sum type + JSON wire protocol. Pure data layer.
├── parser.rs     # Typed-paren parser → ONode tree
├── eval.rs       # Applicative-order leaves-up evaluator + render_child dispatch
├── process.rs    # ProcessRegistry: one subprocess per (lang, env_id) key
├── scheduler.rs  # AutonomousScheduler: concurrent topological Nix-family dispatch + DiskCache
├── nix_ops.rs    # Inline Nix expression evaluation
├── nixos_ops.rs  # NixOS test driver integration
└── bin/
    └── olangc.rs # AOT compiler: .O source → self-contained native binary
```

The evaluator is **applicative order, leaves-up**: inner expressions are
evaluated before the outer expression that contains them, exactly like
normal arithmetic evaluation. A backend that needs to control its own
child evaluation (like `O^` and `quote^`) implements the optional
`eval_ast` hook to take over.

Backend shims (`backends/`) are subprocess scripts. The Rust runtime
communicates with them over **newline-delimited JSON IPC**:

```
Runtime → shim:  {"cmd":"exec","env_id":0,"body":"...","scope":{...}}
Shim → runtime:  {"status":"ok","value":{"t":"int","v":42}}
                 {"status":"err","message":"..."}
```

`html`, `O`, `quote`, and `nix_expr` are handled entirely inline in
`eval.rs` — no subprocess.

Shim resolution order for language tag `<lang>` under `shim_dir`:
`<lang>_shim.py` → `<lang>_shim` → `<lang>.py` → `<lang>`

### Python reference implementation (`o_lang/`) — for cross-validation

```
o_lang/
├── ovalue.py              # OValue tagged union
├── parser.py              # Typed-paren parser
├── evaluator.py           # Leaves-up tree evaluator + env registry
├── cli.py                 # python -m o_lang entry point
└── backends/
    ├── base.py            # Backend base class
    ├── python_backend.py
    ├── html_backend.py
    ├── markdown_backend.py
    ├── latex_backend.py
    ├── text_backend.py
    ├── nix_backend.py
    ├── nix_store_backend.py
    ├── nixos_test_backend.py
    ├── o_backend.py
    └── quote_backend.py
```

---

## Reference

### Registered backends

| Tag          | Shim / handler          | Notes |
|--------------|-------------------------|-------|
| `O`          | inline (`eval.rs`)      | Sequencing block. Evaluates children left-to-right; returns single value, `OList`, or `ONull`. |
| `python`     | `python_shim.py`        | Real `exec()`, persistent globals per env. Returns `__oval_result__`, trailing expr, or captured stdout. Supports `O.eval`/`O.quote`. |
| `html`       | inline (`eval.rs`)      | Body returned as `OHtml`. Blobs → `data:` URL `<img>` tags. |
| `markdown`   | inline (`eval.rs`)      | Body returned as `OStr`. Markup passthrough with value splicing. |
| `latex`      | inline (`eval.rs`)      | Body returned as `OStr`. Passthrough with value splicing. |
| `text`       | inline (`eval.rs`)      | Body returned as `OStr`. Plain passthrough. |
| `quote`      | inline (`eval.rs`)      | Captures body as `OExpr` without evaluating. |
| `nix`        | `nix_shim.py`           | Evaluates Nix expressions via `nix-instantiate`. |
| `nix_expr`   | inline (`nix_ops.rs`)   | Captures body as lazy `ONixExpr` (no evaluation yet). |
| `nix_store`  | `nix_store_shim.py`     | Materialises a Nix derivation, returns `OStorePath`. |
| `nixos_test` | `nixos_test_shim.py`    | NixOS VM test driver. |
| `bash`       | shim (stub)             | Returns code text as `OStr`. Real executor is future work. |
| `shell`      | shim (stub)             | Alias for `bash`. |
| `rust`       | shim (stub)             | Returns code text. Real executor is future work. |
| `racket`     | shim (stub)             | Returns code text. Real executor is future work. |

**Adding a new language (Rust runtime):** write `backends/<lang>_shim.py`
implementing `exec` / `cleanup` / `ping`; add the tag to
`registered_backends` in `src/main.rs`; add a `render_child` branch in
`eval.rs` if the language needs non-default value embedding.

### OValue wire format

Every value that crosses a language boundary is serialized as JSON with a
`"t"` discriminant:

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
{"t":"expr","src":"<O source text>"}
{"t":"nix_expr","body":"...","fingerprint":"...","deps":[...]}
{"t":"derivation","drv_path":"/nix/store/....drv","outputs":["out"],"deps":[...]}
{"t":"request","kind":{...},"source":{...},"fingerprint":"..."}
{"t":"thunk","body":"...","fingerprint":"...","deps":[...]}
{"t":"system","profile_path":"/nix/var/nix/profiles/system"}
```

`store_path`, `expr`, `nix_expr`, `derivation`, `request`, `thunk`, and
`system` are Rust-edition extensions not in the Python MVP. `html` is
supported by both runtimes.

---

## Running the tests

```bash
# Rust unit tests
cargo test

# Python reference implementation tests
python -m tests.test_parser
python -m tests.test_evaluator

# Integration smoke tests (requires cargo build first)
./test_o_lang_examples.sh
```

---

## `olangc` — AOT compiler

`olangc` compiles a `.O` file into a self-contained native binary. The
binary embeds the program source, all backend shim scripts, and the O-lang
runtime. The language runtimes the program *uses* (Python, Nix, etc.) must
still be installed on the target machine.

```bash
cargo build --bin olangc

olangc examples/hello.O                              # output: ./hello
olangc examples/hello.O -o mybin                     # explicit output name
olangc examples/hello.O --shim-dir ./backends --keep-build-dir
```

---

## Status

**v0.1.0** — Rust runtime primary, Python reference implementation for
cross-validation.

Implemented and working:
- Typed-paren parser; all registered backends
- Applicative-order leaves-up evaluator with `render_child` dispatch
- Persistent environments per `(language, index)` pair
- `let` bindings and `$var` splicing
- `{lazy}` / `{defer}` block attributes; `lazy()` / `now()` builtins
- `quote^` + `O.eval` homoiconicity across languages
- Four-rung Nix lattice: `nix_expr` → `instantiate` → `realise` → `activate`
- OS-as-participant (`OSystem`, `current_system()`)
- `autonomous()` scheduler with disk-backed result cache
- `olangc` AOT compiler (self-contained binary output)
- Shebang support

Known limitations (see `SPEC.md` for full details):
- `bash`, `shell`, `rust`, `racket` backends are stubs (parse, do not execute)
- `O.eval` scope does not see top-level `let` bindings from the calling document
- Autonomous scheduling is implemented; cross-document dependency tracking is future work

See `SPEC.md` for the formal language specification.

---

## License

This is research scaffolding for the .O idea. Use it, extend it, break it.
