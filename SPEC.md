# O-lang Specification (v0.2.0)

O-lang is a meta-language whose syntactic unit ( the _typed expression_ ) carries
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
               | "\" "$" IDENT       -- literal $IDENT, not a VarRef splice
```

The `\$IDENT` escape is important when writing real target-language code inside block
bodies. O-lang parses `$IDENT` everywhere (including inside `bash^`, `python^`, etc.)
as a binding reference (VarRef). To write a bare shell variable like `$PATH` in a
`bash{cap=runner}^(...)_bash{cap=runner}` block without triggering an
"Undefined variable" error, write `\$PATH`.
O-lang strips the backslash and passes `$PATH` verbatim to the bash backend.

An `IDENT` that is **not in the registered-language set** is NOT treated as an
opener, even if followed by `^(`. This keeps inner-language code safe -
`2 ^ (x+1)` in a Python body does not parse as a `2^(…)_2` expression.

Aliases: `py → python`, `md → markdown`, `tex → latex`, `plain → text`, `o → O`.
Aliases are resolved to their canonical tag before backend dispatch.

### 1.1 Environment markers

`python[0]^(...)_python[0]` and `python[1]^(...)_python[1]` are two **separate
persistent environments** for the same language. State (imports, bindings,
function definitions) lives inside the env and survives across every
expression that references that env in the document.

`python^(...)_python` without brackets is ephemeral. It receives a fresh
backend process for that expression and is cleaned up afterward. Persistent
state is always explicit through `[n]`. The parser also requires the opener
and closer to match textually. If you opened with `python^(` you must close
with `)_python`; if you opened with `python[0]^(` you must close with
`)_python[0]`.

---

## 2. Evaluation semantics

The evaluator is **applicative order, leaves-up** (standard Lisp eval order).
`ONode` is syntax only. Before any document executes, the Rust runtime lowers
the parsed forest to executable OIR and builds a validated `ExecutionPlan`.

### 2.1 Default flow (splice + evaluate)

For each OIR `Exec` instruction `E` with language `L[n]`, embedded
`BackendInterface`, and body `B = [c₁, c₂, …]`:

1. For each child `cᵢ`:
    * if `cᵢ` is text, emit `cᵢ` verbatim into a splice buffer.
    * if `cᵢ` is an ExpressionNode, recursively evaluate it to get OValue `vᵢ`,
      then emit `E.backend.renderer(vᵢ)` into the splice buffer.
2. Concatenate the splice buffer → final source string `B*`.
3. Return `backend(L).evaluate(B*, env(L, n))`.

Persistent environments: `env(L, n)` is created exactly once for an explicit
environment marker and memoized in the process registry. Subsequent references
to the same `(L, n)` pair reuse the same process. A bare expression uses the
ephemeral environment identifier and is destroyed after dispatch.

### 2.2 Structural backends (`eval_ast` hook)

Some backends need control over how their children are evaluated because the
default splice-then-evaluate flow is wrong for them. Their OIR
`BackendInterface` uses `inline_ast`, and the OIR evaluator hands the entire
region to its structural executor.

Two backends currently use this:

* **`O^(...)_O`** is the host and sequencing backend. It evaluates each OIR
  child exactly once in planned source order and returns the last non-null
  value. Whitespace-only text between children is formatting and does not
  replace the result. `let` bindings inside the region extend its lexical
  child scope.

* **`quote^(...)_quote`** captures its OIR body as parseable O source without
  evaluating any child. The resulting OExpr can be passed to `O.eval(expr)`
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
`EvalContext` - any env bindings the quoted expression references see
current state. `O.quote(src_str)` parses a source fragment and returns
it as an unevaluated `OExpr`, so Python code can build up O source
programmatically and eval it.

`O.eval(...)` receives a lexical snapshot of the O scope visible at the
backend call site. The fragment can read caller `let` bindings, including
bindings established earlier in the current typed expression. The fragment
evaluates through OIR with a cloned root scope, so `let` bindings created by
the callback do not mutate the caller.

The explicit form is:

```
let snapshot = scope()
python^(O.eval(expr, $snapshot))_python
```

`scope()` returns an `OScope` containing a detached copy of the current O-level
bindings. `O.eval(expr, snapshot)` MUST reject values that are not OScope and
MUST use the supplied bindings instead of the callback-site snapshot. Python's
`O.scope()` captures the current O bindings; `O.scope(dict)` constructs a
restricted OScope from the supplied entries. Scope writes during evaluation
never mutate either the snapshot or its source scope.

### 2.4 Environment lifetime and forcing contract

Environment lifetime and request forcing are part of the stable language core:

- A persistent environment is keyed by `(language, env_id)` and lives until the
  evaluator drops it or explicitly cleans it up.
- Bare `lang^(...)_lang` blocks are ephemeral. Explicit `lang[n]` blocks are
  persistent.
- `now(request)` forces exactly the named deferred computation.
- `now(group)` resolves the group according to its `GroupMode`.
- Under `Policy::Autonomous`, schedulable Nix requests and dry activation are
  buffered first and forced at explicit force points (`now(...)`,
  `autonomous(...)` exit, document end). Eval and real activation stay on the
  evaluator thread because they require live process state or mutate the host
  profile.
- A backend counts as **pure** only when the runtime may safely reuse a cached
  result for identical `(body, deps, env)` input. Unknown backends are
  conservatively impure.

### 2.5 Executable OIR and plan contract

Every authoritative Rust execution entry point follows this path:

```text
.O source -> ONode -> OIrProgram -> validated ExecutionPlan -> OIR evaluator
```

This includes the `O` interpreter, REPL entries, notebook cells,
`olangc --target script`, `o-link --run`, compiled hosted binaries, and source
fragments received through `O.eval`.

OIR instructions are `Text`, `Load`, `Store`, `Invoke`, and `Exec`. Each Exec
owns a `BackendInterface` containing its canonical backend name, purity,
splice renderer, and execution mode. Each Invoke owns an `InvokeMode` that is
eager, lazy, autonomous, or one of the four group modes. Runtime dispatch,
policy regions, and `{lazy}` cache validation use this embedded metadata rather
than reconstructing policy from independent name tables after lowering.

ExecutionPlan contains structural, sequence, and data edges. Before execution
it MUST validate node identities, edge bounds, root coverage, root uniqueness,
and graph acyclicity. The evaluator MUST obtain both top-level and direct-child
execution order from the plan. Policy-changing Invoke instructions such as
`lazy`, `autonomous`, `batch`, `all`, `any`, and `race` retain control over how
their planned child regions are evaluated.

---

## 3. OValue: the canonical intermediate value

OValue is a tagged union. Every typed expression evaluates to one. All
inter-language data passing goes through this type - it is the runtime
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
          | OScope  { bindings: {str: OValue} } -- detached lexical snapshot
          | OBlob   { data: bytes, mime: str }
          | OExpr   { src: str }               -- homoiconicity: quoted O source
          | ONixExpr { body: str, fingerprint: str, deps: [OValue] }
          | ODerivation { drv_path: str, outputs: [str] }
          | ORequest { kind: RequestKind, source: OValue }
          | OThunk   { body: str, fingerprint: str, deps: [OValue] }
          | OSystem  { profile_path: str }
          | OGroup   { mode: GroupMode, members: [OValue], fingerprint: str }
```

`GroupMode` is one of `batch`, `all`, `any`, `race` - the execution topology
of an `OGroup` (see §3.1).

Two additional system-facing forms freeze the runtime boundary:

```
         | OCapability { kind: CapabilityKind, identity: str, metadata: {str: OValue} }
         | OSnapshot   { kind: SnapshotKind, identity: str, state: {str: OValue} }
```

- `OCapability` is an authority-bearing handle to a privileged resource (file,
  memory region, device, clock, network endpoint, process, service, or system
  activation). A live capability identity is an opaque bearer resolved through
  a private session table. Metadata is descriptive and grants no authority.
- `OSnapshot` is an inert observation of world state captured at a boundary
  (for example a system generation, service state, or filesystem view). It is
  the persistable counterpart to live references such as `OSystem`.

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
| `scope`       | `{"t":"scope","bindings":{...}}`             | Explicit lexical root for O.eval      |
| `blob`        | `{"t":"blob","v":"<base64>","mime":"..."}`   |                                       |
| `expr`        | `{"t":"expr","src":"<O source text>"}`       | Quoted O expression; send to O.eval   |
| `nix_expr`    | `{"t":"nix_expr","body":"...","fp":"..."}`   | Rust ext; lazy Nix expression         |
| `group`       | `{"t":"group","mode":"batch","members":[...],"fingerprint":"..."}` | Rust ext; coordination group |
| `capability`  | `{"t":"capability","kind":"service","identity":"...","metadata":{...}}` | Authority-bearing system handle |
| `snapshot`    | `{"t":"snapshot","kind":"system","identity":"...","state":{...}}` | Persistable captured state |
| `error`       | `{"t":"error","msg":"..."}` | Rust ext; error outcome value (used in Batch results) |

### 3.0 Runtime-boundary contract

The language/runtime contract distinguishes three classes of values:

- **Pure values** - inert data that is serializable, replayable, and safe to
  persist across boots (`ONull`, numbers, strings, lists, maps, blobs,
  `OExpr`, `ONixExpr`, `ODerivation`, `OThunk`, `OSnapshot`, most `ORequest`s).
- **Referential values** - live handles into the world whose identity is stable
  as a reference but whose observed state may change (`OSystem`).
- **Effectful values** - authority-bearing, scope, or orchestration values whose meaning
  depends on execution context (`OCapability`, `OScope`, `OGroup`, `OError`, and effectful
  `ORequest`s such as `activate`).

The runtime MUST preserve these invariants:

1. Every `OValue` is wire-serializable.
2. Only pure values are assumed replay-safe across time.
3. Referential values are hashable by handle identity, not by live state.
4. Values that encode authority or world mutation MUST NOT be treated as
   boot-persistable system facts merely because they serialize.
5. A future OS/runtime layer may add richer kinds, but must classify them into
   this same boundary model.

### 3.0.1 System activation authority

System activation has separate mutating and dry forms:

```
activate(store_path [, profile])
dry_activate(store_path [, profile])
activate(system_activation_capability, store_path [, profile])
```

`activate(path[, profile])` requests a real switch and uses the ambient host
authority of the current process, matching what the same user could do from
Bash. `dry_activate(path[, profile])` runs `switch-to-configuration
dry-activate`. If the first argument is a live `system_activation` capability
issued by the current Evaluator, it is an embedding-specific profile guard. The
evaluator checks that private authority table when constructing the guarded
request and again when forcing it. A serialized, forged, revoked,
cross-evaluator, or wrong-profile explicit capability MUST be rejected before
the perform boundary.

### 3.0.2 Hosted backend authority

Hosted backend effects are ambient host-language effects by default. The source
form is:

```
LANG^(body)_LANG
```

The evaluator MUST make all grantable backend rights available to shim
backends: `fs_read`, `fs_write`, `network`, and `process`. This is the normal
O-lang execution substrate, not a privilege exception. The older
`LANG{cap=NAME,RIGHT,...}^` spelling remains accepted for compatibility and
embedding experiments, but ordinary source MUST NOT require a host-injected
backend grant to access backend capacities.

Deferred Eval requests MUST carry a backend authority identity and right set
and MUST revalidate them immediately before force. In the default evaluator that
identity is the process-local wildcard authority minted at Evaluator startup.

A persistent backend process MUST be keyed by its complete authority policy in
addition to language and environment number. Process reuse MUST NOT cross
authority policies.

Python has no adapter-required source authority. Bash and shell require
`process`. Adapters that compile or launch a target program require
`fs_write` and `process`. Nix evaluators require `fs_read`, `fs_write`,
`network`, and `process`. These requirements are part of `BackendInterface`
and therefore part of executable OIR rather than an evaluator-side name table.
An unregistered shim MUST default to the full authority set. A public or
deserialized OIR program MUST NOT weaken registered adapter requirements;
execution MUST reject an embedded interface that differs from registry policy.

The Rust runtime applies backend policy through Python audit hooks. On macOS it
also installs an operating-system sandbox profile around shim processes. The
default policy is intentionally permissive; these layers are policy plumbing,
not a claim that ordinary O execution contains host-language effects.

### 3.1 Coordination groups

An `OGroup` makes the **execution topology** of several computations explicit
in the value model. Where an `ORequest` names a single deferred computation, a
group names a collection of them plus a `mode` that says how they relate:

| Builtin            | Mode    | Resolution                                                |
|--------------------|---------|-----------------------------------------------------------|
| `batch(a, b, …)`   | `batch` | Run all members; collect every outcome including failures |
| `all(a, b, …)`     | `all`   | Every member must succeed; any failure aborts the group   |
| `any(a, b, …)`     | `any`   | Redundancy/fallback; yields the first member to succeed   |
| `race(a, b, …)`    | `race`  | Latency competition; yields the first member to settle    |

A group performs no work on its own - it is a control value. It is forced by
`now(group)`, by `autonomous(group)`, or at document end under Autonomous
policy.

### 3.2 Group Semantics

#### Construction

`batch`, `all`, `any`, and `race` are **special forms**, not ordinary
functions. Their arguments are evaluated under a capture policy:

- under `Policy::Eager`, members are evaluated as `Policy::Lazy` so request
  chains are captured instead of forced;
- under `Policy::Lazy`, members remain lazy;
- under `Policy::Autonomous`, members remain autonomous, so request chains are
  captured and buffered for the scheduler.

This means that:

```
batch(realise(instantiate($e1)), realise(instantiate($e2)))
```

always builds:

```
Group(Batch, [Request[Realise(Request[Instantiate(e1)])], ...])
```

even when the surrounding policy is Eager. Inside `autonomous(...)`, the same
captured `ORequest` chains are also recorded in the autonomous buffer before
the scheduler flushes.

#### Member evaluation policy

Group members may be already-resolved values, deferred `ORequest`s captured
under the constructor's active capture policy, or nested groups. The group's
`fingerprint` composes from the mode and the **ordered** member content
identities - member order is semantically significant and is never sorted.

#### Empty group behavior

A group with no members is rejected at construction time; calling
`batch()`/`all()`/`any()`/`race()` with zero arguments is a hard error.

#### Batch result shape

`batch(a, b, …)` returns an `OList` with exactly one element per input member,
in declaration order, for ordinary Fresh-mode resolution. Failures are NOT
fatal in that path: a member that fails is wrapped as an `OValue::Error` in the
list so every input slot has a corresponding output slot. Callers can
distinguish success from failure by testing `is_error()` on each element.

Under `CacheMode::Strict`, used after an autonomous scheduler flush, a cache
miss is not an ordinary member failure. It is a scheduler invariant failure and
remains a hard error even for `batch`.

#### All failure behavior

`all(a, b, …)` is a hard all-or-nothing barrier. If **any** member fails, the
entire group fails immediately and propagates the first error. Unlike `batch`,
there is no error wrapping - `all` either returns a full `OList` of successes,
or returns an error.

#### Any failure behavior

`any(a, b, …)` returns the **first member to succeed**. Members are tried in
source order; a failing member is skipped and the next is tried. `any` fails
only when every member has failed, in which case the last error is propagated
with an aggregate message noting how many members failed.

#### Race settlement behavior

`race(a, b, …)` returns the **first result to settle**, whether it is a success
or a failure. In sequential mode, the first member always settles first;
remaining members are not consulted. In concurrent mode (when members are
threadable Nix-family Requests), the first channel message wins and remaining
threads run to completion but their results are discarded.

> **Note:** Race does not yet cancel remaining work. After the winner is
> selected, other threads continue executing and are silently dropped. Full
> cancellation (via cooperative tokens or subprocess kill) is a future feature.

#### Nested group behavior

Group members may themselves be groups. `resolve_member` recurses into nested
groups with the same `CacheMode`, so:

```
all(any(a, b), batch(c))
```

resolves as `[first_success_of(a,b), [c]]` - inner topologies are respected.

#### Cancellation status

Race does not cancel losers. Any and Race over concurrent members may leave
background threads running after the winner is selected. Cooperative
cancellation is planned for a future release.

#### Cache behavior

Two resolution modes are used internally:

- `CacheMode::Fresh` - force each member via the active executor; used by
  `now(group)`. This is the standard "resolve right now" path.
- `CacheMode::Strict` - read each member from the scheduler/eval cache; a
  cache miss is a hard error. Used after `autonomous(...)` flush to verify the
  scheduler materialized every buffered request.

#### Scheduler guarantees

Under `autonomous(batch(…))`:

1. The inner requests are buffered (not executed) while evaluating the body.
2. At block exit, the autonomous scheduler flushes the buffer concurrently,
   up to `scheduler.parallelism` threads at a time.
3. Results are written to the L1 memory cache and the L2 disk cache.
4. The batch group is resolved from the cache using `CacheMode::Strict` - a
   cache miss after flush is a hard error, not a silent fallback.

`now(group)` uses the same `resolve_group` path but with `CacheMode::Fresh`,
dispatching threadable members in batches capped at `scheduler.parallelism`.

Design principles:

* **Structurally rich, semantically minimal.** Values carry data and
  self-description (the tag), but no methods and no behavior.
* **`OBlob` carries its own mime type.** This is how a matplotlib figure
  becomes an `<img>` in HTML without either side understanding the other's
  type system - `image/png` is the contract.
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
  re-enter the evaluator at runtime - e.g. Python uses it to power
  `O.eval(expr)`.
* `eval_ast(node, ctx)` _(optional)_: take full control of child
  evaluation, skipping the default splice flow. Required for `O` (which
  sequences) and `quote` (which captures without evaluating).

`render_child` is a consumer projection, not the OValue lifting map. Its
fidelity is classified as typed (`T`), structural with an erased O tag (`S`),
human presentation (`P`), or opaque marker (`O`):

| OValue family | Python | Nix | HTML | LaTeX | Markdown | Default |
|---------------|--------|-----|------|-------|----------|---------|
| Null, bool, int, float, string | T | T | P | P | P | S |
| HTML, store path | T | S | P | P | P | O |
| List, map | T | T | P | P | P | S |
| Scope | T | O | O | O | O | O |
| Blob | S | S | P | P | P | O |
| Expr | T | S | P | P | P | O |
| NixExpr | T | T | P | P | P | O |
| Derivation, system | T | S | P | P | P | O |
| Thunk | T | O | O | O | O | O |
| Error | T | O | P | P | P | O |
| Request, capability, snapshot, group | T | O | O | O | O | O |

Container fidelity MUST be no stronger than the least faithful child.
Implementations MUST classify every OValue variant for every registered
renderer. An opaque value MUST render an identifying marker rather than
silently disappearing.

The Python renderer uses `OOpaqueValue` for O-specific tagged values with no
native Python form. That handle MUST retain the complete wire object and MUST
round-trip it unchanged. Deserializing such a handle MUST NOT create a private
capability broker binding.

---

## 5. Output rendering

An O document evaluates to a single root OValue. The CLI's final rendering
step converts that OValue to the user-requested target format:

* `--as auto` (default): target format is determined by the root expression's
  own language - an `html^(…)_html` root prints HTML, a `markdown^(…)_markdown`
  root prints Markdown. When the root is `O^(...)_O`, the format is
  inherited from the **first substantive inner expression**.
* `--as html | markdown | latex | text`: force that backend's `render_child` as
  the final rendering step, regardless of root language.
* `--as json`: dump the OValue JSON - useful for debugging.

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
| `bash`       |         | `backends/bash_shim.py`. Executes Bash and returns stdout. |
| `shell`      |         | `backends/shell_shim.py`. Executes POSIX sh and returns stdout. |
| `rust`       |         | `backends/rust_shim.py`. Compiles with rustc, runs, and returns stdout. |
| `racket`     |         | `backends/racket_shim.py`. Executes Racket and returns stdout. |
| `cpp`        |         | Compiles C++17 with g++, runs, and returns stdout. |
| `csharp`     |         | Compiles and runs through .NET or Mono. |
| `haskell`    |         | Executes through runghc or ghc. |
| `lisp`       |         | Executes through Guile, Chicken, or Chez Scheme. |
| `common_lisp`|         | Executes through SBCL, ECL, CLISP, or CCL. |
| `sql`        |         | Executes against persistent in-memory SQLite per environment. |
| `ruby`       |         | Executes with Ruby. |
| `matlab`     |         | Executes with Octave or MATLAB. |
| `mathematica`|         | Executes with WolframScript. |
| `webassembly`|         | Converts WAT when needed and runs with Wasmtime or Wasmer. |
| `java`       |         | Compiles with javac and runs with java. |
| `javascript` |         | Executes with Node.js. |
| `ocaml`      |         | Interprets or compiles with the OCaml toolchain. |

### Python reference implementation (`o_lang/`)

Same core tag set minus Rust-only orchestration extensions.
`quote` and `O` are structural backends implemented via `eval_ast`.
Let-bindings (`let NAME = LANG^(...)`) and `$var` substitution are supported.

Adding a new language: write a Backend subclass, add it to
`o_lang/backends/__init__.py::default_registry`, and add the tag to
`REGISTERED_LANGUAGES` in `parser.py`.

---

## 7. Known limitations / current status

* **`$var` splice** is supported for top-level `let` bindings in both runtimes.
  Variable references inside nested typed-expression bodies in the Rust runtime
  work through lexical scope passed into OIR `Exec`. The Python ref impl
  similarly threads scope through `_eval_expression`.
* **Async coordination.** `{lazy}` and `{defer}` attributes create
  deferred Requests (Thunks). Lazy requests can be auto-forced by a splice;
  deferred requests require explicit `now()`. `lazy(…)` and `autonomous(…)`
  switch policy for their planned child region. When `now(group)` is called,
  Nix-family Request members (`Instantiate`, `Realise`) and dry activation are
  dispatched as concurrent threads; real activation remains evaluator-local;
  `any(…)` returns the first success, `race(…)` returns the first result
  (success or failure). Eval Requests and nested plain values resolve serially.
  Full cancellation and async I/O are future work.
* **`O.eval` same-environment recursion.** The callback uses a caller snapshot
  or explicit OScope and keeps callback writes local. A callback
  cannot execute the same persistent backend environment that is currently
  waiting for the callback result; nested backend work must use another
  environment index.
* **Optional runtime dependencies.** Executing shims report an explicit error
  when their target interpreter or compiler is not installed locally.
* **Python ref impl** (`o_lang/`): maintained as a readable reference and test
  harness. The Rust runtime is the authoritative implementation.

---

## 8. Versioning

This spec is v0.2.0. The v0.2 bump reflects:
- `OExpr` wire format and `quote^` / `O.eval` semantics are now implemented in both runtimes.
- `OValue` wire format table expanded with all Rust-runtime types.
- `let` binding and `$var` substitution work in both runtimes.
- Registered-language table updated with all registered backends and their
  implementation status.

Breaking changes to OValue or the Backend protocol
will bump the minor version until v1.0.

Author: Lee Ostadi
