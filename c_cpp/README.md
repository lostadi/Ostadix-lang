# O-lang — C edition (easy native build)

Pure C17 implementation of the O-lang meta-language runtime and `olangc` AOT compiler.

> **Every expression carries its own interpreter as part of its syntax.**

This edition requires only a C compiler (`cc` / `clang` / `gcc`) and `make`. No Rust, no CMake for the common path.

## Quickstart

```bash
cd c_cpp

# Build interpreter + compiler (one command)
make

# Run a program (python^ blocks use the shared ../backends/*.py shims)
./O ../examples/hello.O ../backends
# → 2

./O ../examples/meta_eval.O ../backends
# (demonstrates quote^ + O.eval homoiconicity across languages)

# AOT compile to a self-contained native binary
./olangc ../examples/hello.O -o /tmp/hello_c
/tmp/hello_c
# → 2

# The produced binary is native C code; it still needs python3 (and nix if your
# .O program uses nix^) on the machine where you *run* it — exactly like a
# program that calls out to Python.
```

## Easy compile

- `make` — builds `O` and `olangc`
- `make clean`
- `make test` — smoke-tests core examples (hello, bindings, meta_eval, html_basic)
- `make olangc-test` — also exercises the AOT path
- `make run EX=meta_eval` — quick run of an example

Everything is built from the `.c` files in `src/` + headers in `include/`. The Makefile is deliberately simple (no subdirs, no generated build system).

## What works

- Full typed-paren grammar (`LANG^( ... )_LANG`, `[n]` envs, `{lazy}/{defer}`, `let`, `$var`, calls)
- `O^`, `quote^`, `html^`, `markdown^`/`latex^`/`text^` (inline)
- `python^` via the real `python_shim.py` (persistent envs, `__oval_result__`, trailing expr, stdout capture, `O.eval` + `O.quote`)
- Basic builtins: `now()`, `lazy()`, `instantiate()`, `realise()`, `activate()`, `current_system()`, `autonomous()`
- Nix rung (when `nix` is in PATH)
- Shebang stripping
- `olangc` AOT that produces a single native binary embedding the runtime + shims + your program

See `SPEC.md` (in repo root) for the language specification.

## Requirements

- C17 compiler + make (macOS: Xcode Command Line Tools; Linux: gcc/clang + make)
- `python3` (for any `python^` / `py^` blocks)
- Optional: `nix` (for `nix*` examples and the four-rung lattice)

## olangc (AOT)

`olangc` turns a `.O` file into a native executable that contains:

- the O-lang C evaluator compiled in
- your program source
- the backend shim scripts (extracted at startup to a private temp dir)

```bash
./olangc myprog.O -o myprog
./myprog
```

The binary has **no dependency on the olangc tool or the source tree** at runtime.

## Layout

```
c_cpp/
├── Makefile          # the easy build
├── include/          # public + internal C headers
├── src/
│   ├── value.c       # OValue + JSON wire (core)
│   ├── parser.c      # typed-paren parser + AST
│   ├── process.c     # shim subprocess mgmt + JSON IPC
│   ├── eval.c        # leaves-up evaluator, splice, structural backends, render_child
│   ├── scheduler.c   # (serial for MVP) autonomous + disk cache
│   ├── nix_ops.c     # instantiate / realise / activate
│   ├── main.c        # the `O` interpreter
│   └── olangc.c      # the AOT compiler
└── README.md         # this file
```

## Adding a language (for hackers)

1. Add the tag to the registered list in `main.c` / `olangc` generated mains.
2. Implement a `_shim.py` (or native executable) speaking the newline-JSON protocol.
3. Add a `render_*` case in `eval.c` if the language needs special `render_child` rules.
4. For structural behaviour (like `O` / `quote`), handle in `eval_typed_expr`.

See the Python reference implementation and `backends/` for examples.

## Status & limitations

- Matches the core of the Rust edition for the documented examples.
- `O.eval` round-trips work (python only).
- Stub shims (bash, rust, racket, shell) return the code text as a string (same as other editions).
- Some advanced scheduler / concurrent Nix behaviour is serial in this port.
- See root `SPEC.md` and `README.md` for the full feature set and known limitations.

## License

Research scaffolding. Use it, extend it, break it.
