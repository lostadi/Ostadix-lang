# General HGraph executor plan

This is a next-stage design document, not a claim about the current release.
The current evaluator projects OIR into HGraph, solves and schedules it, checks
the projected root order, and still executes arbitrary evaluator operations via
the serial OIR executor in `src/eval.rs`. A general HGraph executor would replace
that final serial dispatch only after the obligations below are met.

## Current grounding

- Persistent hosted processes are owned by `ProcessRegistry` in `src/process.rs`,
  keyed today by `(language, env_id, BackendSandboxPolicy)`.
- HGraph schedule derivation in `src/hgraph/schedule.rs` converts structural,
  sequence, data, group, request, cache, and `ActorSerial` relations into
  topological clusters.
- HGraph actor identity is currently represented as `ActorId { lang, env }` in
  `src/hgraph/graph.rs`; that is sufficient for analysis but too coarse for a
  safe executor.

## Executor invariants

1. **Actor identity is explicit and generation-aware.** Each persistent
   evaluator instance is an actor identified by at least `(canonical language,
   implementation/runtime identity, environment ID, process generation)`.
   `ProcessRegistry` already owns live child processes; the graph executor must
   lift that ownership into an actor table so a restarted Python environment is
   not confused with the prior process that used the same `python[0]` syntax.

2. **Same-actor operations are serialized.** Operations targeting the same live
   evaluator actor must execute in program-order-compatible sequence. The
   existing HGraph `ActorSerial` edge in `src/hgraph/schedule.rs` is the analysis
   seed; the executor must enforce it at runtime with per-actor mailboxes or
   equivalent queues.

3. **Independent actors may run concurrently only when all constraints permit.**
   Two operations may run in parallel only if structural, sequence, data, effect,
   and authority constraints all permit it. The current scheduler accounts for
   structural, sequence, data, group, request, cache, and actor edges; a general
   executor must add explicit effect and authority edges before treating a
   cluster as parallel-safe.

4. **Ephemeral evaluator instances get unique identities.** Ephemeral blocks
   currently use `u32::MAX` in OIR. A graph executor must allocate a fresh actor
   identity for every ephemeral evaluation, including a unique generation, so
   there is no accidental sharing or serialization collapse between unrelated
   ephemeral shims.

5. **No unsafe cross-thread process registry sharing.** `ProcessRegistry`
   contains live child process handles and buffered pipes. It must not be shared
   across threads via unsafe trait assertions; specifically, do not add unsafe
   `Send` or `Sync` implementations to make the current registry fit a parallel
   executor. Use actor-owned threads, channels, or a single owner that drives
   processes from explicit messages.

6. **Completion materializes OValue outputs and releases dependent hyperedges.**
   Every completed operation must publish its `OValue` to the graph node that
   represents the operation result. Dependent hyperedges become ready only after
   all input OValue materializations and authority checks are complete.

7. **Error and final-result semantics are deterministic.** If multiple
   operations fail, select the reported error by first error in stable schedule
   order, not by wall-clock completion order. Cancellation should be best-effort:
   after the selected error is known, stop launching unscheduled work and send
   cancellation to running actors where supported; already-committed external
   effects remain observable and must be documented.

8. **Tests compare serial and graph execution observationally.** A scoped
   effect-safe fragment should run through both `execute_plan_serial` and the
   graph executor. Tests must compare final OValues, visible scope updates,
   stable traces, backend process reuse where applicable, and deterministic
   errors.

9. **Benchmarks isolate cost centers.** Benchmarks must report scheduler
   overhead, evaluator startup, OValue boundary conversion, and backend execution
   separately. Otherwise parallel speedups could merely hide process startup or
   serialization overhead.

## Semantic obligations

### Persistent process ownership

The executor must define which component owns each live backend process.
`ProcessRegistry` currently creates, reuses, and cleans up processes. A graph
executor can preserve this by giving each actor an owner task that receives
commands over channels, but ownership must remain singular so pipe ordering,
cleanup, and restart generation are unambiguous.

### External effects

Shim backends can read files, write files, use the network, spawn processes, or
observe mutable runtime state. The graph must represent these effects before
parallelizing them. Until effect fingerprints are complete, effectful shims
should serialize conservatively or require explicit authority/effect annotations
that prove independence.

### Deterministic error selection

Parallel completion order is nondeterministic. User-visible errors should be
selected by stable schedule order: derive a total order from the HGraph schedule
and node IDs, report the earliest failed node in that order, and attach later
failures as secondary diagnostics only if doing so is stable.

### Cancellation

Cancellation is a semantic boundary, not just an optimization. The executor
should stop admitting new work after the deterministic selected failure, cancel
or drain running actor operations according to backend capability, and always
clean up ephemeral actors. Persistent actors that might be left in an unknown
state after cancellation should be generation-bumped or restarted.

### Schedule equivalence

For the effect-safe fragment, graph execution must be observationally equivalent
to serial OIR execution. Equivalence includes final OValue, root selection,
scope mutation, lazy/defer forcing behavior, capability failures, and stable
error choice. Schedule equivalence does not require identical internal trace
timing.

## Staged implementation outline

1. **Actor identity model.** Extend HGraph actor metadata from `(lang, env)` to a
   runtime-aware, generation-aware identity. Include ephemeral IDs.
2. **Actor runtime abstraction.** Wrap `ProcessRegistry` ownership behind an
   actor interface that can execute one operation at a time and return `OValue`
   or structured error.
3. **Effect and authority edges.** Add explicit HGraph edges for backend
   authority and declared effects. Keep unknown effects serial.
4. **Ready-queue executor.** Implement a deterministic ready queue over
   `src/hgraph/schedule.rs` clusters. Launch only nodes whose inputs,
   actor mailbox, effects, and authority checks are ready.
5. **Materialization layer.** Store completed OValues on graph nodes and release
   dependent hyperedges. Preserve current root-result semantics.
6. **Deterministic failure and cancellation.** Implement first-error-in-schedule
   order selection, best-effort cancellation, actor cleanup, and generation bump
   for tainted persistent actors.
7. **Serial-vs-graph tests.** Start with inline pure backends, then add isolated
   persistent Python actors, then add deferred Eval requests and groups.
8. **Benchmarks and regression gates.** Add benchmark suites that separate
   scheduler, startup, conversion, and backend execution costs before enabling
   graph execution by default.
