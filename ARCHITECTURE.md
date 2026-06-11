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
├── backends/         # Language shims (Python, Bash, Nix, Racket, Rust, Shell)
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

O-lang processes code through a 5-stage pipeline:

1. **Parse** — Tokenize source into typed expression trees. Each expression
   carries a language tag (e.g., `python`, `html`, `nix`).

2. **Evaluate** — Recursively evaluate inner expressions first (applicative
   order). Child results become available to parent expressions.

3. **Render** — Convert child `OValue` results into the parent language's
   native syntax for interpolation.

4. **Dispatch** — Send the rendered source to the appropriate backend shim
   as a subprocess, communicating via JSON over stdin/stdout.

5. **Cache** — Memoize expensive operations (especially Nix
   instantiate/realise) to avoid redundant work.

## Intermediate Representation (OIR)

`src/ir.rs` provides a thin IR/backend interface layer — a stable seam
between syntax (`ONode`), execution planning (`OIr`), runtime values
(`OValue`), and backend capabilities (`BackendSpec`):

- **`OIr` / `OIrProgram`** — a lowered, backend-neutral form of a parsed
  program. Lowering (`OIrProgram::lower`) is a 1:1 structural mapping of
  the `ONode` forest: `RawText → Text`, `VarRef → Load`,
  `LetBinding → Store`, `Call → Invoke`, `TypedExpr → Exec`.
- **`BackendSpec` / `BackendRegistry`** — centralized backend metadata:
  purity (whether `{lazy}` may cache results), the splice-rendering
  strategy used by `render_child`, and shim path resolution
  (`<dir>/<lang>_shim.py`, `<dir>/<lang>_shim`, `<dir>/<lang>.py`,
  `<dir>/<lang>`, in that order).

The evaluator still walks `ONode` directly; OIR is currently a
debug/inspection surface (`olangc --target ir`) and the designated home for
future execution planning. There is deliberately no native codegen,
optimizer, SSA, or VM at this layer.

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
| `OHtml`        | HTML fragment                        |
| `OStorePath`   | Nix store path                       |
| `ONixExpr`     | Unevaluated Nix expression           |
| `ODerivation`  | Nix derivation                       |
| `OBlob`        | Binary data                          |
| `OExpr`        | Unevaluated O expression             |
| `ORequest`     | HTTP/system request                  |
| `OThunk`       | Deferred computation                 |

## Backend Shims

Each supported language has a shim script in `backends/` that:
- Reads JSON input from stdin
- Evaluates the expression in the target language
- Writes JSON output to stdout

Currently supported: Python, Bash, Nix, Racket, Rust, Shell.

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

`olangc` supports three compilation targets, selected via `--target`:

| Target   | Flag              | Output                              |
|----------|-------------------|-------------------------------------|
| `binary` | `--target binary` | Native ELF/Mach-O binary on disk    |
| `script` | `--target script` | In-process execution (no disk file) |
| `ir`     | `--target ir`     | Lowered OIR dump on stdout          |

**Target A — Binary** (default): creates a temporary Cargo project that
bundles the .O source, runtime, and backend shims, then compiles it with
`cargo build --release`.  The result is a self-contained native binary.

**Target B — Script**: parses and evaluates the .O program directly inside
the `olangc` process.  The evaluator machine code is already loaded into
executable memory as part of the running `olangc` binary — calling it is
semantically equivalent to emitting code into an `mmap`'d executable buffer
and invoking a function pointer.  No intermediate build step or disk binary
is produced.

**Target C — IR**: parses the program with the same front end, lowers the
`ONode` forest to OIR (`src/ir.rs`), and prints the lowered program to
stdout.  A debugging/inspection target — nothing is executed and no output
file is produced.

```bash
# Compile to a binary (Target A)
cargo run --bin olangc -- examples/hello.O -o hello

# Execute in-process (Target B)
cargo run --bin olangc -- examples/hello.O --target script

# Dump the lowered OIR (Target C)
cargo run --bin olangc -- examples/hello.O --target ir
```

## Implementations

| Edition | Directory | Status     |
|---------|-----------|------------|
| Rust    | `src/`    | **Active** |
| C17     | `c_cpp/`  | Complete   |
| Python  | `o_lang/` | Reference  |
