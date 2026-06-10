# O-lang Specification (v0.2.0)

O-lang is a meta-language whose syntactic unit — the _typed expression_ — carries
its own interpreter as part of its syntax. Every expression declares which
language it is written in; the runtime dispatches evaluation to a backend for
that language; the resulting value is a language-neutral `OValue` that can be
consumed as an atom inside expressions written in any other language.

The guiding thesis: **evaluator choice is a structural property of the
expression, not a global setting.** This is Lisp's homoiconicity (code and data
share syntactic form) generalized across multiple languages.

---

## 1. Grammar

```
document     ::= body_part*
body_part    ::= text | expression
expression   ::= OPENER body_part* CLOSER           -- matching CLOSER required
OPENER       ::= IDENT ("[" DIGITS "]")? "^("
CLOSER       ::= ")_" IDENT ("[" DIGITS "]")?       -- must match OPENER's IDENT
                                                    --   and env marker exactly
IDENT        ::= [A-Za-z_][A-Za-z0-9_]*            -- AND registered as a language
text         ::= (any char | backslash-escape)+     -- everything not OPENER/CLOSER
backslash-escape
             ::= "\" OPENER          -- literal opener, no expression
               | "\" CLOSER          -- literal closer, not a termination
```

An `IDENT` that is **not in the registered-language set** is NOT treated as an
opener, even if followed by `^(`. This keeps inner-language code safe —
`2 ^ (x+1)` in a Python body does not parse as a `2^(…)_2` expression.

Aliases: `py → python`, `md → markdown`, `tex → latex`, `plain → text`, `o → O`.
Aliases are resolved to their canonical tag before backend dispatch.

### 1.1 Environment markers

`python[0]^(...)_python[0]` and `python[1]^(...)_python[1]` are two **separate
persistent environments** for the same language. State (imports, bindings,
function definitions) lives inside the env and survives across every
expression that references that env in the document.

`python^(...)_python` without brackets is shorthand for `python[0]^(...)_python` —
they share the same default env. BUT: the parser requires the opener and
closer to match _textually_. If you opened with `python^(` you must close with
`)_python`; if you opened with `python[0]^(` you must close with `)_python[0]`.

---

## 2. Evaluation semantics

The evaluator is **applicative order, leaves-up** (standard Lisp eval order).

### 2.1 Default flow (splice + evaluate)

For each ExpressionNode `E` with language `L[n]` and body `B = [c₁, c₂, …]`:

1. For each child `cᵢ`:
    * if `cᵢ` is text, emit `cᵢ` verbatim into a splice buffer.
    * if `cᵢ` is an ExpressionNode, recursively evaluate it to get OValue `vᵢ`,
      then emit `backend(L).render_child(vᵢ)` into the splice buffer.
2. Concatenate the splice buffer → final source string `B*`.
3. Return `backend(L).evaluate(B*, env(L, n))`.

Persistent environments: `env(L, n)` is created exactly once via
`backend(L).make_env()` and memoized in the `EvalContext`. Subsequent
references to the same `(L, n)` pair reuse the same env object.

### 2.2 Structural backends (`eval_ast` hook)

Some backends need control over how their children are evaluated — the
default splice-then-evaluate flow is wrong for them. A backend can
optionally implement `eval_ast(node, ctx) -> OValue` to take over. The
evaluator checks for this method and, if present, hands over control
entirely. Otherwise it falls back to the default flow.

Two backends currently use this:

* **`O^(...)_O`** — the *host / sequencing* backend. Evaluates its
  children left-to-right in source order, returning the children's
  OValues as an `OList` (or a single value if the list has length 1, or
  `ONull` if empty). Whitespace-only text between children is treated
  as formatting and dropped; non-whitespace text is preserved as
  `OStr`. This is the canonical wrapper for full .O scripts; it lets
  side-effects in `python[0]^(...)_python[0]` blocks flow naturally
  down the page.

* **`quote^(...)_quote`** — captures its body as an `OExpr` *without
  evaluating it*. If the body is exactly one ExpressionNode, the
  quoted AST is that node; otherwise the body is wrapped in a
  synthetic O-node. The companion operator is `O.eval(expr)` available
  inside Python blocks.

### 2.3 Homoiconicity: `quote^` + `O.eval`

The `quote^` / `O.eval` pair gives O Lisp-style code-as-data,
generalized across target languages:

```
O^(
  python[0]^(
    q = quote^(python^(6 * 7)_python)_quote      # q : OExpr
  )_python[0]
  python[0]^(
    O.eval(q)                                    # -> 42 (OInt)
  )_python[0]
)_O
```

An `OExpr` value spliced into a Python body is bound as a live object
(not `repr()`'d), so user code can `O.eval(expr)` against the *live*
`EvalContext` — any env bindings the quoted expression references see
current state. `O.quote(src_str)` parses a source fragment and returns
it as an unevaluated `OExpr`, so Python code can build up O source
programmatically and eval it.

---

## 3. OValue: the canonical intermediate value

OValue is a tagged union. Every typed expression evaluates to one. All
inter-language data passing goes through this type — it is the runtime
embodiment of the canonical intermediate form any lossless polyglot system
must have.

```
OValue  ::= ONull
          | OBool   { value: bool }
          | OInt    { value: int }
          | OFloat  { value: float }
          | OStr    { value: str }
          | OHtml   { value: str }              -- trusted HTML fragment
          | OStorePath { path: str }            -- Nix store path
          | OList   { items: (OValue, …) }
          | OMap    { pairs: ((str, OValue), …) }
          | OBlob   { data: bytes, mime: str }
          | OExpr   { src: str }               -- homoiconicity: quoted O source
          | ONixExpr { body: str, fingerprint: str, deps: [OValue] }
          | ODerivation { drv_path: str, outputs: [str] }
          | ORequest { kind: RequestKind, source: OValue }
          | OThunk   { body: str, fingerprint: str, deps: [OValue] }
          | OSystem  { profile_path: str }
          | OGroup   { mode: GroupMode, members: [OValue], fingerprint: str }
```

`GroupMode` is one of `batch`, `all`, `any`, `race` — the execution topology
of an `OGroup` (see §3.1).

The Rust runtime uses the JSON wire format with a `"t"` discriminant:

| Tag           | Wire form                                    | Notes                                 |
|---------------|----------------------------------------------|---------------------------------------|
| `null`        | `{"t":"null"}`                               |                                       |
| `bool`        | `{"t":"bool","v":true}`                      |                                       |
| `int`         | `{"t":"int","v":42}`                         |                                       |
| `float`       | `{"t":"float","v":3.14}`                     |                                       |
| `str`         | `{"t":"str","v":"hello"}`                    |                                       |
| `html`        | `{"t":"html","v":"<p>...</p>"}`              | Rust ext; trusted HTML fragment        |
| `store_path`  | `{"t":"store_path","path":"/nix/store/..."}`  | Rust ext; Nix store path              |
| `list`        | `{"t":"list","v":[...]}`                     |                                       |
| `map`         | `{"t":"map","v":{...}}`                      |                                       |
| `blob`        | `{"t":"blob","v":"<base64>","mime":"..."}`   |                                       |
| `expr`        | `{"t":"expr","src":"<O source text>"}`       | Quoted O expression; send to O.eval   |
| `nix_expr`    | `{"t":"nix_expr","body":"...","fp":"..."}`   | Rust ext; lazy Nix expression         |
| `group`       | `{"t":"group","mode":"batch","members":[...],"fingerprint":"..."}` | Rust ext; coordination group |

### 3.1 Coordination groups

An `OGroup` makes the **execution topology** of several computations explicit
in the value model. Where an `ORequest` names a single deferred computation, a
group names a collection of them plus a `mode` that says how they relate:

| Builtin            | Mode    | Resolution                                                |
|--------------------|---------|-----------------------------------------------------------|
| `batch(a, b, …)`   | `batch` | Run all for throughput; yields an `OList` of every result |
| `all(a, b, …)`     | `all`   | Fan-out, every member must succeed; yields every result   |
| `any(a, b, …)`     | `any`   | Redundancy/fallback; yields the first member to succeed   |
| `race(a, b, …)`    | `race`  | Latency competition; yields the first member to settle    |

A group performs no work on its own — it is a control value. It is forced by
`now(group)`, by `autonomous(group)`, or at document end under Autonomous
policy. `batch`/`all` collect **all** member results into a list (member order
preserved); `any`/`race` yield a **single** winning member.

Members may be already-resolved values, deferred `ORequest`s (when built under
`lazy(…)`), or nested groups. The group's `fingerprint` composes from the mode
and the **ordered** member content identities — member order is semantically
significant, so it is never sorted.

`autonomous(batch(…))` is the MVP scheduler integration: the inner requests are
buffered, the scheduler dispatches the independent ones concurrently when the
block exits, and the batch resolves into a list of results from the cache.

> **MVP note.** The runtime is synchronous (no async, no Tokio, no Eval-request
> parallelism, no cancellation). `any` and `race` therefore both resolve members
> left-to-right and return the first success; the mode is preserved in the value
> so a future concurrent scheduler can honour the true topology.

Design principles:

* **Structurally rich, semantically minimal.** Values carry data and
  self-description (the tag), but no methods and no behavior.
* **`OBlob` carries its own mime type.** This is how a matplotlib figure
  becomes an `<img>` in HTML without either side understanding the other's
  type system — `image/png` is the contract.
* **`OExpr` gives meta-level homoiconicity.** An O program can produce an
  O AST as a value and have it evaluated. Lisp's `quote`/`eval` generalized
  across multiple target languages.

---

## 4. Backend protocol

Every language backend implements:

```python
class Backend:
    name: str                                            # canonical language tag
    def make_env(self) -> Env: ...                       # fresh persistent env
    def render_child(self, v: OValue) -> str: ...       # embed as my source
    def evaluate(self, body: str, env, ctx=None) -> OValue: ...
    # OPTIONAL structural hook (see 2.2):
    def eval_ast(self, node, ctx) -> OValue: ...         # take over child eval
```

* `make_env()`: called once per unique `(L, n)` pair.
* `render_child(v)`: decide how a foreign value looks _as source code of my
  language_. HTML's `render_child(OBlob(png, "image/png"))` returns
  `<img src="data:image/png;base64,…">`. Python's `render_child(v)` returns
  `repr(to_python(v))` so the spliced value is a valid Python literal.
* `evaluate(body, env, ctx=None)`: run / render / transform the body string
  into an OValue. The optional `ctx` parameter gives access to the live
  `EvalContext` (env registry, backend lookup) for backends that need to
  re-enter the evaluator at runtime — e.g. Python uses it to power
  `O.eval(expr)`.
* `eval_ast(node, ctx)` _(optional)_: take full control of child
  evaluation, skipping the default splice flow. Required for `O` (which
  sequences) and `quote` (which captures without evaluating).

---

## 5. Output rendering

An O document evaluates to a single root OValue. The CLI's final rendering
step converts that OValue to the user-requested target format:

* `--as auto` (default): target format is determined by the root expression's
  own language — an `html^(…)_html` root prints HTML, a `markdown^(…)_markdown`
  root prints Markdown. When the root is `O^(...)_O`, the format is
  inherited from the **first substantive inner expression**.
* `--as html | markdown | latex | text`: force that backend's `render_child` as
  the final rendering step, regardless of root language.
* `--as json`: dump the OValue JSON — useful for debugging.

### 5.1 `O^` root: sequence rendering

When the root expression is `O^(...)_O` and it evaluates to an
`OList` (multiple children), each item is rendered independently via
the target backend's `render_child` and then concatenated with `\n`.
This is the difference between a document that happens to be a list
of HTML fragments (which should become a single HTML document) versus
a list literal being rendered (which should become `<ul>...</ul>`).

This decouples source format from output format: the same `.O` file can render
to multiple targets by invocation. The source is the expression tree; the
output is one rendering of that tree.

---

## 6. Currently-registered languages

### Rust runtime (`src/main.rs`)

| Tag          | Aliases | Backend behavior                                              |
|--------------|---------|---------------------------------------------------------------|
| `python`     | `py`    | Real execution via `backends/python_shim.py`. Persistent globals per env. Returns last expr. Supports `O.eval`/`O.quote`. |
| `html`       |         | Inline: body returned as `OHtml`. `render_child` makes blobs into data URLs. |
| `markdown`   | `md`    | Inline: body returned as `OStr`. Markup passthrough with value splicing. |
| `latex`      | `tex`   | Inline: body returned as `OStr`. Passthrough with value splicing. |
| `text`       | `plain` | Inline: body returned as `OStr`. Passthrough. |
| `O`          | `o`     | Inline: sequences children left-to-right; returns last non-null value. |
| `quote`      |         | Inline: captures body as `OExpr` without evaluating. No subprocess. |
| `nix`        |         | `backends/nix_shim.py`. Evaluates Nix expressions. |
| `nix_expr`   |         | Inline: captures body as lazy `ONixExpr` (deferred Nix eval). |
| `nix_store`  |         | `backends/nix_store_shim.py`. Materialises a store path. |
| `nixos_test` |         | `backends/nixos_test_shim.py`. NixOS integration test runner. |
| `bash`       |         | `backends/bash_shim.py` (stub — returns code text). Replace with real executor. |
| `shell`      |         | `backends/shell_shim.py` (stub). |
| `rust`       |         | `backends/rust_shim.py` (stub). |
| `racket`     |         | `backends/racket_shim.py` (stub). |

### Python reference implementation (`o_lang/`)

Same tag set minus `nix_expr`, `nix_store`, `nixos_test`, stubs.  
`quote` and `O` are structural backends implemented via `eval_ast`.  
Let-bindings (`let NAME = LANG^(...)`) and `$var` substitution are supported.

Adding a new language: write a Backend subclass, add it to
`o_lang/backends/__init__.py::default_registry`, and add the tag to
`REGISTERED_LANGUAGES` in `parser.py`.

---

## 7. Known limitations / current status

* **`$var` splice** is supported for top-level `let` bindings in both runtimes.
  Variable references inside nested typed-expression bodies in the Rust runtime
  work via the `scope` dict passed through `eval_typed_expr`. The Python ref
  impl similarly threads scope through `_eval_expression`.
* **Eager evaluation only.** `{lazy}` and `{defer}` attributes create deferred
  Requests (Thunks) that are auto-forced when spliced or explicitly forced via
  `now()`. `lazy(…)` and `autonomous(…)` switch the policy for their argument;
  `autonomous(batch(…))` dispatches independent Nix-family requests concurrently
  through the scheduler. Coordination groups (`batch`/`all`/`any`/`race`, see
  §3.1) make execution topology explicit, but the runtime is still synchronous —
  full async scheduling, `any`/`race` parallelism, and cancellation are future
  work.
* **`O.eval` scope limitation.** `eval_source` (called on `O.eval`) creates a
  fresh document scope; top-level `let` bindings from the calling document are
  NOT visible inside the evaluated fragment. Variables in persistent backend
  envs (Python subprocess globals) ARE accessible because they live in the
  subprocess, not the Rust scope.
* **Stub shims**: `bash`, `shell`, `rust`, `racket` are registered and parse
  correctly, but their shims return the code text as `OStr` (not executed).
  Real executor shims are future work.
* **Python ref impl** (`o_lang/`): maintained as a readable reference and test
  harness. The Rust runtime is the authoritative implementation.

---

## 8. Versioning

This spec is v0.2.0. The v0.2 bump reflects:
- `OExpr` wire format and `quote^` / `O.eval` semantics are now implemented
  (not future work) in both runtimes.
- `OValue` wire format table expanded with all Rust-runtime types.
- `let` binding and `$var` substitution work in both runtimes.
- Registered-language table updated with all registered backends and their
  implementation status.

Breaking changes to OValue or the Backend protocol
will bump the minor version until v1.0.
