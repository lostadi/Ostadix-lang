# Ostadix-lang Architecture

Ostadix-lang is a universal polyglot expression framework where every expression
syntactically declares which language it is written in.

```
html^( <p>Result: python^( 2 + 2 )_python</p> )_html
```

## Repository Layout

```
Ostadix-lang/
├── src/              # Rust implementation (primary, active)
│   ├── main.rs       #   CLI entry point
│   ├── lib.rs        #   Library crate root
│   ├── parser.rs     #   Tokenizer & expression parser
│   ├── ir.rs         #   OIR intermediate representation & backend registry
│   ├── eval.rs       #   Recursive evaluator
│   ├── effects.rs    #   Semantic effect/resource summaries
│   ├── executor/     #   Graph coordinator and serial oracle
│   ├── hgraph/       #   Directed value/state/control hypergraph
│   ├── value.rs      #   OValue universal type system
│   ├── process.rs    #   Subprocess management for backends
│   ├── nix_ops.rs    #   Nix build/realise operations
│   ├── nixos_ops.rs  #   NixOS-specific operations
│   ├── scheduler.rs  #   Parallel evaluation scheduler
│   ├── ocore/        #   O-core compiler (lexer, typeck, HIR, MIR, codegen)
│   ├── live_system/  #   Hosted Live-World package/service oracle
│   ├── kernel_world.rs #  KernelWorld foreign-kernel manifest contract & oracle
│   ├── world/        #   Shared governed identities/effects foundation
│   ├── project/      #   Route-preserving project bundle lifting
│   └── bin/          #   Additional binary targets (olangc, ocorec, olink, …)
├── ocore/            # Native systems runtime and bootable x86_64 kernel proof
├── okernel-multikernel/ # Foreign-kernel personality proposal & boot-and-test entrypoint
├── backends/         # Language shims (Python, Bash, Nix, Racket, Rust, … — see README backend table)
├── examples/         # .O example programs
├── c_cpp/            # Complete C17 port (standalone)
├── o_lang/           # Legacy Python prototype (reference only)
├── tests/            # Rust integration tests plus Python-era legacy tests
├── fuzz/             # Parser fuzz targets and seed corpus
├── setup/            # Cross-platform bootstrap scripts
├── scripts/          # Repository management scripts
├── docs/             # Design documents and brainstorms
├── boot-and-test.sh  # One phased entrypoint over every build/boot/test layer
├── SPEC.md           # Language specification
└── README.md         # Project overview
```

## Evaluation Pipeline

Ostadix-lang processes hosted code through a 6-stage pipeline:

1. **Parse** — Tokenize source into typed expression trees. Each expression
   carries a language tag (e.g., `python`, `html`, `nix`).

2. **Lower** — Convert the syntax-only `ONode` forest to executable OIR.
   Every `Exec` instruction freezes the backend's canonical identity, purity,
   splice renderer, and dispatch mode.

3. **Plan** — Build and validate `ExecutionPlan`. Structural edges connect
   children to parents, sequence edges preserve source order, and data edges
   connect loads to their visible stores.

4. **Project and execute** — Lower executable operations into a directed HGraph.
   Ordinary results, resource versions, actor state, and successful completion
   are nodes. Operations are directed hyperedges and become runnable exactly
   when every input node is materialized. `O_EXECUTOR=serial` retains the
   topological OIR interpreter as the differential reference semantics.

5. **Render, authorize, and dispatch** - Convert child values with the renderer
   embedded in OIR, resolve the block's live backend capability against the
   adapter's required rights, then run an inline value handler or send source
   to a policy-keyed backend shim.

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

## Directed HGraph execution

`src/hgraph/from_oir.rs` derives one semantic effect summary before constructing
each executable edge. Every executable edge has one distinguished OValue output,
one successful-completion output, and zero or more successor resource-state
outputs. Its inputs include ordinary child/data values plus the prior versions
of every resource it accesses. Persistent shim operations also consume and
produce `ActorState(canonical-language[environment])`.

Unknown hosted effects read and write `HostWorld`, which is a conservative
umbrella for host-observable state. The graph does not infer exact filesystem or
network footprints from arbitrary hosted source. Source `reads=`, `writes=`,
and `serial=host` declarations can add constraints, but cannot erase an unknown
fallback. Likewise, `effects=pure` cannot upgrade an arbitrary shim into trusted
worker-pool work.

The governed-world identity foundation in `src/world/` now shares all 20
constitutional identity atoms with `ocore/world/identity.oc`. It separates a
World snapshot epoch from independently generated node, domain, process,
resource, object, and task-attempt identity. The bounded `OWIDENT` v1 corpus is
an identity-only byte oracle; serialized capability IDs remain descriptive
non-authority. The separate `OWPROTO` v1 foundation adds deterministic
architecture-independent records, strict 16 KiB and caller-selected bounds,
four fixed kinds, canonical nested identities, and offline schema negotiation.
Its 20-record, 1254-byte Rust/`.oc` corpus is a codec oracle, not a stream or
network transport, live handshake, authenticated session, authority channel,
OValue envelope, receipt, Governor, or consensus implementation. Mode 29 adds a
separate self-framed `OWVALUE` v1 layer rather than changing those four
`OWPROTO` v1 kinds. The bounded portable layer has a 4096-byte record maximum,
depth-16 and 128-node limits, an explicit allowlist, canonical ordered records
and scalar-key maps, a root-only inert versioned extension with a recursively
portable payload, and SHA-256 over the complete canonical record. Rust and
native `.oc` share a fixed 19-record, 928-byte oracle whose concatenated
SHA-256 is `264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc`,
and hosted projection rejects
authority-bearing, capsule, live-reference, request, and other effectful forms.
Mode 30 adds the separate self-framed `OWRECEIPT` v1 bounded execution-receipt
layer. It binds descriptive World identities and generations, content digests,
capability-right descriptions, terminal/commit fields, evidence-gate identity,
and an algorithm-tagged signature envelope into canonical records and
domain-separated signing preimages. Rust and native `.oc` converge on the fixed
two-record, 3,239-byte corpus (SHA-256
`1edd90bf881cd42d08e2031482baae4e7c9a95bd78cfa65f0cbe14147c0a2604`) and its
1,575-byte current and 1,546-byte stale signing preimages. A pinned public
conformance key drives real
hosted Ed25519 sign/verify and tamper tests. Native `.oc` converges on receipt and
signing-preimage bytes and rejects malformed signature envelopes, but does not
claim a general freestanding Ed25519 verifier or trusted signer policy. The
offline corpus is not yet emitted by the HGraph, project, live-system,
KernelWorld, O-Git, or World evidence paths.
The hosted PR6 `ResourceKey` vocabulary now distinguishes World, Governor,
node, domain, process, generic resource, object, descriptive capability,
namespace, task-attempt, artifact-publication, device, and accelerator state.
Each class carries the existing validated World identity type rather than an
unvalidated string. Device and accelerator access expands to the canonical
generic resource key as well, giving all three views one shared HGraph state
dependency. It never aliases ambient `HostWorld`.

This vocabulary describes governed state without pretending that arbitrary
hosted work has become mediated. User-authored effect declarations cannot mint
the governed classes, no production lowering emits them yet, and `HostWorld`
remains the residual umbrella for ambient host effects. The optional grounding
identity is caller-supplied. Underlying identity helpers compare caller pairs;
grounding checks the bound World epoch and membership only, not a live
authoritative nested-generation snapshot. This type/effect foundation is not a
wire format or native Mode 31 and does not implement a distributed Governor,
membership transport, resource registry, `/world` namespace, remote execution,
placement, device assignment, DMA/IOMMU isolation, or current-epoch
enforcement. It passes no G0--G13 gate.
The native product boundary and G0--G13
dependency ladder are fixed in
[`docs/OSTADIX_WORLD.md`](docs/OSTADIX_WORLD.md) and mechanically classified by
[`evidence/world_alpha_gates.toml`](evidence/world_alpha_gates.toml). Hosted
implementations remain useful only under the non-qualifying
[`Hosted World Reference Profile`](docs/HOSTED_WORLD_REFERENCE_PROFILE.md).

Ordinary source sequence is lowered as a predecessor completion-token input.
That dependency is omitted only for direct members of an explicit concurrent
group, or when both operations are verified, deterministic, infallible,
resource-free inline renderers from the trusted `html`, `markdown`, `text`, and
`latex` set, each complete structural subtree contains only literal text and
recursively trusted renderers, and neither operation is a child of a structural
`O` sequencing region. Unknown facts preserve sequence. Resource chains can
still order members of a concurrent group when their effects conflict.

`ReadySchedule` derives blockers only from producers of directed operation
inputs. The coordinator materializes all outputs atomically after success,
recomputes the frontier after each completion, and emits no completion or
successor-state token after failure. Deterministic commit order does not stand
in for effect ordering. Parallel worker dispatch remains limited to the
verified pure inline renderer class. Broader read sharing and precise resource
models are future optimizations.

## Universal Value System (OValue)

Every value crossing language boundaries is represented as one of these types:

| Type           | Purpose                                             |
|----------------|-----------------------------------------------------|
| `ONull`        | Absence of value                                    |
| `OBool`        | Boolean true/false                                  |
| `ONumber`      | Arbitrary-precision integers, rationals, and floats |
| `OText`        | Text with explicit encoding metadata                |
| `OChar`        | Single Unicode scalar value                         |
| `OHtml`        | HTML fragment                                       |
| `OList`        | Ordered heterogeneous collection                    |
| `OMap`         | String-keyed value mapping                          |
| `OSeq`         | Sequence with source-language shape metadata        |
| `OObject`      | Structural object with deterministic string fields  |
| `OEntriesMap`  | Map with arbitrary OValue keys                      |
| `OSet`         | Set preserving ordered/unordered source intent      |
| `OSymbol`      | Interned symbolic identifier                        |
| `OKeyword`     | Keyword value                                       |
| `OScope`       | Detached lexical binding snapshot                   |
| `OBlob`        | Binary data with MIME type                          |
| `OBytes`       | Structural byte value                               |
| `OGraph`       | Value graph frame for shared identity and cycles    |
| `ONative`      | Language-native capsule with rehydration policy     |
| `OStorePath`   | Nix store path                                      |
| `ONixExpr`     | Unevaluated Nix expression                          |
| `ODerivation`  | Nix derivation                                      |
| `OExpr`        | Unevaluated O expression                            |
| `ORequest`     | Deferred computation with compositional fingerprint |
| `OThunk`       | Captured backend body for Eval requests             |
| `OGroup`       | Explicit execution topology                         |
| `OError`       | Captured failed outcome (produced by `batch`)       |
| `OSystem`      | Live OS/profile reference                           |
| `OCapability`  | Authority-bearing resource handle                   |
| `OSnapshot`    | Persistable captured world state                    |

This table describes the rich hosted `src/value.rs::OValue` carrier. It is not
the Mode 29 portable allowlist. Conversion into `src/world/value.rs` is
fallible and rejects authority, capsules, live references, executable or
deferred work, and other effectful hosted variants. The hosted canonical-CBOR
shim protocol is unchanged by `OWVALUE` v1.

The runtime boundary is intentionally split:

- **Pure values** can be cached, replayed, and persisted.
- **Referential values** identify live world entities by handle, not snapshot.
- **Effectful values** carry authority or orchestration meaning and must be
  handled explicitly by schedulers and persistence layers.

OValue lifting and source rendering are distinct boundaries. Lifting maps a
backend result into the tagged OValue carrier. `render_child` projects that
carrier into one consumer language and can be typed, structural, presentational,
or opaque. The exhaustive fidelity matrix is specified in [SPEC.md](SPEC.md)
and implemented by `RenderFidelity`; opaque control and authority values emit
visible markers instead of silently falling through.

Live OCapabilities are not validated from their serialized fields. The hosted
O-core `CapabilityBroker` maps a 256-bit operating-system-random bearer to a
kernel generation-tagged handle in a private session table, then checks kind
and rights before transport. The evaluator uses the same rule for hosted
system activation: a private table maps a live bearer to one authorized
profile. Capability metadata is descriptive only.

Hosted backend effects follow the same rule. `BackendAuthorityBroker` binds a
live bearer to one backend language and a subset of `fs_read`, `fs_write`,
`network`, and `process`. Required adapter rights are embedded in
`BackendInterface`, additional source rights are block attributes, and the
evaluator validates their union before dispatch and before deferred force.
The process registry key includes the full sandbox policy, which prevents a
wider persistent environment from serving a narrower block.

Unprivileged `activate(path[, profile])` constructs a dry activation request.
Mutating `activate(capability, path[, profile])` requires a live
`system_activation` bearer and is checked both at construction and at force
time. Real activation stays on the evaluator thread rather than entering the
autonomous disk-cached scheduler.

## Backend Shims

Each supported language has a shim script in `backends/` that:
- Reads length-prefixed canonical CBOR input from stdin
- Evaluates the expression in the target language
- Writes length-prefixed canonical CBOR output to stdout

Python shims run under an audit policy derived from the validated capability.
On macOS the shim also runs under an operating-system sandbox profile. Bash,
compiled-language, and Nix adapters declare the rights inherent in invoking
their target tool, so those blocks require an explicit host capability even
when the source lists no additional right.

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

`olangc` supports five compilation targets, selected via `--target`:

| Target   | Flag              | Output                              |
|----------|-------------------|-------------------------------------|
| `binary` | `--target binary` | Native ELF/Mach-O binary on disk    |
| `wasm`   | `--target wasm`   | `wasm32-wasip1` module on disk     |
| `script` | `--target script` | In-process execution (no disk file) |
| `ir`     | `--target ir`     | OIR, plan, and textual HGraph dump  |
| `dot`    | `--target dot`    | Graphviz DOT hypergraph on stdout   |

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

**Target E — Dot**: parses and lowers to OIR, then builds the full
`HGraph` hypergraph (`src/hgraph/`) from that OIR, runs the type solver, and
serialises the result as a Graphviz DOT digraph on stdout. Ordinary values,
resource versions, actor-state versions, and completion/control values have
distinct styles. Executable and constraint hyperedges are explicit vertices,
so input-to-operation and operation-to-output port direction remains visible.
Nothing is executed and no output file is produced.

```bash
# Compile to a binary (Target A)
cargo run --bin olangc -- examples/hello.O -o hello

# Compile to WASI (Target B)
cargo run --bin olangc -- examples/hello.O --target wasm -o hello.wasm

# Execute in-process (Target C)
cargo run --bin olangc -- examples/hello.O --target script

# Dump the lowered OIR (Target D)
cargo run --bin olangc -- examples/hello.O --target ir

# Emit Graphviz DOT hypergraph (Target E)
cargo run --bin olangc -- examples/hello.O --target dot

# Render to PNG via Graphviz
cargo run --bin olangc -- examples/hello.O --target dot | dot -Tpng -o graph.png
```

## Implementations

| Edition | Directory | Status     |
|---------|-----------|------------|
| Rust    | `src/`    | **Active** |
| C17     | `c_cpp/`  | Complete   |
| Python  | `o_lang/` | Reference  |
