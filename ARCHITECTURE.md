# O-lang Architecture

O-lang is a universal polyglot expression framework where every expression
syntactically declares which language it is written in.

```
html^( <p>Result: python^( 2 + 2 )_python</p> )_html
```

## Repository Layout

```
O-lang/
├── src/              # Rust implementation (primary, active)
│   ├── main.rs       #   CLI entry point
│   ├── lib.rs        #   Library crate root
│   ├── parser.rs     #   Tokenizer & expression parser
│   ├── ir.rs         #   OIR intermediate representation & backend registry
│   ├── eval.rs       #   Recursive evaluator
│   ├── value.rs      #   OValue universal type system
│   ├── process.rs    #   Subprocess management for backends
│   ├── nix_ops.rs    #   Nix build/realise operations
│   ├── nixos_ops.rs  #   NixOS-specific operations
│   ├── scheduler.rs  #   Parallel evaluation scheduler
│   └── bin/          #   Additional binary targets
├── ocore/            # Native systems runtime and bootable x86_64 kernel proof
├── backends/         # Language shims (Python, Bash, Nix, Racket, Rust, … — see README backend table)
├── examples/         # .O example programs
├── c_cpp/            # Complete C17 port (standalone)
├── o_lang/           # Legacy Python prototype (reference only)
├── tests/            # Python-era test suite (legacy, for o_lang/)
├── setup/            # Cross-platform bootstrap scripts
├── tools/            # Development utilities (markdown extraction)
├── scripts/          # Repository management scripts
├── docs/             # Design documents and brainstorms
├── SPEC.md           # Language specification
└── README.md         # Project overview
```

## Evaluation Pipeline

O-lang processes hosted code through a 6-stage pipeline:

1. **Parse** — Tokenize source into typed expression trees. Each expression
   carries a language tag (e.g., `python`, `html`, `nix`).

2. **Lower** — Convert the syntax-only `ONode` forest to executable OIR.
   Every `Exec` instruction freezes the backend's canonical identity, purity,
   splice renderer, and dispatch mode.

3. **Plan** — Build and validate `ExecutionPlan`. Structural edges connect
   children to parents, sequence edges preserve source order, and data edges
   connect loads to their visible stores.

4. **Execute** — Interpret OIR through the plan's stable topological root
   schedule. Structural OIR regions implement `O` and `quote`; ordinary
   execution regions build splice buffers from child OValues.

5. **Render and dispatch** — Convert child values with the renderer embedded
   in OIR, then run an inline value handler or send source to a backend shim.

6. **Schedule and cache** — Request values created by OIR carry compositional
   fingerprints. The eager executor and autonomous scheduler apply the cache
   and dependency semantics selected by the OIR operation.

## Intermediate Representation (OIR)

`src/ir.rs` is the canonical hosted execution surface. It is the seam between
syntax (`ONode`), executable instructions (`OIr`), dependency planning
(`ExecutionPlan`), runtime values (`OValue`), and typed backend interfaces
(`BackendSpec` / `BackendInterface`):

- **`OIr` / `OIrProgram`** is the executable form of a parsed program.
  Lowering maps `RawText` to `Text`, `VarRef` to `Load`, `LetBinding` to
  `Store`, `Call` to `Invoke`, and `TypedExpr` to `Exec`. `Exec` also owns a
  `BackendInterface`, so runtime dispatch cannot drift from OIR analysis.
  `Invoke` owns an `InvokeMode`, so eager, lazy, autonomous, and group policy
  is decided during lowering rather than rediscovered by the evaluator.
- **`ExecutionPlan`** is the validated dependency graph built from OIR.
  Structural edges encode child to parent dependencies, sequence edges preserve
  left-to-right order, and data edges connect `load $x` to the latest visible
  `store $x`. It rejects invalid identities, out-of-bounds edges, duplicated
  roots, and cycles, then provides the stable topological root schedule and
  direct-child schedules used by the evaluator.
- **`BackendSpec` / `BackendRegistry`** provides centralized backend metadata:
  purity (whether `{lazy}` may cache results), the splice-rendering
  strategy used by `render_child`, typed dispatch mode (`inline_ast`,
  `inline_value`, `shim`), and shim path resolution
  (`<dir>/<lang>_shim.py`, `<dir>/<lang>_shim`, `<dir>/<lang>.py`,
  `<dir>/<lang>`, in that order).

`Evaluator::eval_document` and `eval_document_with_scope` lower immediately to
OIR and call the same OIR engine used by `eval_ir_program`. No production path
interprets `ONode`. `O.eval` callbacks re-enter through the parser, lower to a
new OIR program, validate its plan, and execute it through the same engine. The
callback root scope is a clone of the O bindings visible at the backend call
site. Reads therefore have lexical visibility, while callback `let` bindings
cannot mutate the caller. The evaluator retains the most recent validated plan
through `last_execution_plan()` for inspection and tests.

`scope()` materializes those bindings as a first-class OScope. The Python shim
can send OScope in an `eval_request`, so `O.eval(expr, scope_snapshot)` replaces
implicit capture with an explicit lexical root. OScope is distinct from OMap:
it carries namespace intent and is conservatively non-cacheable,
non-replayable, and non-persistable because its bindings may delegate live
capabilities or references.

OIR remains intentionally distinct from SSA. Recursive OIR regions preserve
lexical scope and policy-changing special forms such as `lazy`, `autonomous`,
and coordination groups. Every `Store`, `Invoke`, and `Exec` maps its direct
OIR children to plan identities before execution. The plan expresses legal
dependency order, while runtime Request values carry fingerprints into the
eager executor or autonomous scheduler.

O-core does not lower into this representation. Native `.oc` files use the
separate `AST -> typed HIR -> SSA MIR -> object` pipeline under `src/ocore/`.
This separation prevents machine-level mutation, layout, and control-flow
semantics from being conflated with OIR's backend dependency graph.

## Universal Value System (OValue)

Every value crossing language boundaries is represented as one of these types:

| Type           | Purpose                              |
|----------------|--------------------------------------|
| `ONull`        | Absence of value                     |
| `OBool`        | Boolean true/false                   |
| `OInt`         | Integer number                       |
| `OFloat`       | Floating-point number                |
| `OStr`         | Text string                          |
| `OList`        | Ordered collection                   |
| `OMap`         | Key-value mapping                    |
| `OScope`       | Detached lexical binding snapshot    |
| `OHtml`        | HTML fragment                        |
| `OStorePath`   | Nix store path                       |
| `ONixExpr`     | Unevaluated Nix expression           |
| `ODerivation`  | Nix derivation                       |
| `OBlob`        | Binary data                          |
| `OExpr`        | Unevaluated O expression             |
| `ORequest`     | Deferred computation / control value |
| `OThunk`       | Deferred computation                 |
| `OGroup`       | Explicit execution topology          |
| `OSystem`      | Live OS/profile reference            |
| `OCapability`  | Authority-bearing resource handle    |
| `OSnapshot`    | Persistable captured world state     |

The runtime boundary is intentionally split:

- **Pure values** can be cached, replayed, and persisted.
- **Referential values** identify live world entities by handle, not snapshot.
- **Effectful values** carry authority or orchestration meaning and must be
  handled explicitly by schedulers and persistence layers.

Live OCapabilities are not validated from their serialized fields. The hosted
O-core `CapabilityBroker` maps a 256-bit operating-system-random bearer to a
kernel generation-tagged handle in a private session table, then checks kind
and rights before transport. The evaluator uses the same rule for hosted
system activation: a private table maps a live bearer to one authorized
profile. Capability metadata is descriptive only.

Unprivileged `activate(path[, profile])` constructs a dry activation request.
Mutating `activate(capability, path[, profile])` requires a live
`system_activation` bearer and is checked both at construction and at force
time. Real activation stays on the evaluator thread rather than entering the
autonomous disk-cached scheduler.

## Backend Shims

Each supported language has a shim script in `backends/` that:
- Reads JSON input from stdin
- Evaluates the expression in the target language
- Writes JSON output to stdout

Shims exist for Python, Bash, Shell, Nix, `nix_store`, `nixos_test`, Racket,
Rust, C#, C++, Haskell, Lisp, Common Lisp, SQL, Ruby, MATLAB, Mathematica,
WebAssembly, Java, JavaScript, and OCaml. These are executing adapters for
their local runtimes. `html`, `markdown`, `latex`, `text`, `quote`,
`nix_expr`, and `O` are handled inline without a subprocess. See the backend
table in README.md for runtime requirements.

## Building & Testing

```bash
# Build
cargo build

# Run an example
cargo run -- examples/hello.O backends

# Run tests
cargo test

# Run example smoke tests
bash test_o_lang_examples.sh
```

## Compiler Targets (`olangc`)

`olangc` supports four compilation targets, selected via `--target`:

| Target   | Flag              | Output                              |
|----------|-------------------|-------------------------------------|
| `binary` | `--target binary` | Native ELF/Mach-O binary on disk    |
| `wasm`   | `--target wasm`   | `wasm32-wasip1` module on disk     |
| `script` | `--target script` | In-process execution (no disk file) |
| `ir`     | `--target ir`     | Lowered OIR dump on stdout          |

**Target A — Binary** (default): creates a temporary Cargo project that
bundles the .O source, runtime, and backend shims, then compiles it with
`cargo build --release`.  The result is a self-contained native binary.

**Target B — WASI**: generates the same hosted runtime project for
`wasm32-wasip1`. Programs remain subject to the subprocess facilities exposed
by their WASI host.

**Target C — Script**: parses and evaluates the .O program directly inside
the `olangc` process.  The evaluator machine code is already loaded into
executable memory as part of the running `olangc` binary — calling it is
semantically equivalent to emitting code into an `mmap`'d executable buffer
and invoking a function pointer.  No intermediate build step or disk binary
is produced.

**Target D — IR**: parses the program with the same front end, lowers the
`ONode` forest to OIR (`src/ir.rs`), and prints the lowered program to
stdout.  A debugging/inspection target — nothing is executed and no output
file is produced.

```bash
# Compile to a binary (Target A)
cargo run --bin olangc -- examples/hello.O -o hello

# Compile to WASI (Target B)
cargo run --bin olangc -- examples/hello.O --target wasm -o hello.wasm

# Execute in-process (Target C)
cargo run --bin olangc -- examples/hello.O --target script

# Dump the lowered OIR (Target D)
cargo run --bin olangc -- examples/hello.O --target ir
```

## Implementations

| Edition | Directory | Status     |
|---------|-----------|------------|
| Rust    | `src/`    | **Active** |
| C17     | `c_cpp/`  | Complete   |
| Python  | `o_lang/` | Reference  |
