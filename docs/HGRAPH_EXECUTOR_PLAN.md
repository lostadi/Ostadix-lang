# State-complete HGraph executor

The Rust hosted runtime executes OIR through a directed HGraph. The graph
coordinator is the default; `O_EXECUTOR=serial` selects the reference OIR
executor used by the differential conformance suite.

## Implemented operation shape

```text
ordinary child/data values ----\
completion dependencies --------> [ Execute(plan node) ] -> ordinary OValue
resource state R@n ------------/                         -> Completion(plan node)
actor state A@n --------------/                          -> resource state R@n+1
                                                         -> actor state A@n+1
```

Every executable hyperedge has one distinguished ordinary result and one
successful-completion output. Resource/control outputs are synthetic scheduling
values and carry no `OValue`. Every output has exactly one producer.

## Effect and state model

`src/effects.rs` derives summaries before executable edges are built. The
currently modeled resource keys are:

- `HostWorld`
- evaluator-local state
- O scope bindings
- project-relative paths
- host paths
- environment variables
- standard I/O
- exact or unknown network endpoints
- named services
- persistent actor state keyed by canonical language and environment number
- exact governed World epochs, node generations, and domain generations
- owner-scoped governed resources, task attempts, and artifact publication state

Unknown hosted operations read and write `HostWorld` and evaluator-local state.
`HostWorld` aliases precise host resource declarations conservatively. A
persistent shim also consumes and produces its actor-state token. The live
process registry does not expose a trustworthy generation, so actor resource
identity does not invent a constant generation field.

Governed resource keys do not alias `HostWorld`: they are vocabulary intended
for a future trusted World/O-core lowering. A key by itself is not proof of
mediation or authority. Source `reads=` and `writes=` declarations cannot
construct these keys, no production lowering emits them yet, and today's
arbitrary hosted backends keep their conservative `HostWorld` dependency.
`olangc file.O --target ir --grounding` renders the distinction,
capability-right requirements, actor and
capsule affinity information, and any residual ambient dependency. Optional
`--world-id NAME --world-epoch N` binds that inspection report to an exact
caller-supplied epoch; it does not consult a live snapshot, enforce freshness,
perform placement, or execute the plan.

Effect attributes are checked constraints. `effects=unknown` can downgrade a
verified renderer. `effects=pure` cannot upgrade an arbitrary shim. `reads=`,
`writes=`, and `serial=host` add dependencies without removing unknown
fallbacks. Backend authority remains a permission model and is not treated as
an exact footprint.

## Sequence and group control

Ordinary source sequence is implemented by adding the earlier operation's
completion token to the later operation's executable inputs. Sequence is
relaxed only in either of these cases:

1. Both operations are direct members of the same explicit `batch`, `all`,
   `any`, or `race` group.
2. Both operations are verified, deterministic, infallible, resource-free
   inline renderers from the trusted `html`, `markdown`, `text`, and `latex`
   set; both complete structural subtrees contain only literal text and
   recursively trusted renderers; and neither operation is a child of a
   structural `O` sequencing region.

If any fact is unknown, the completion dependency remains. Explicit group
topology does not override resource conflicts: unknown members still share the
directed `HostWorld` chain.

## Scheduling and failure

`ReadySchedule` builds a producer map over every ordinary and synthetic output.
An operation's blockers are exactly the producers of its input nodes. There is
no separately maintained actor or effect blocker in the final scheduler.

The coordinator owns the mutable evaluator and process registry. It launches
only verified pure inline renderer tasks on worker threads. After each success,
it materializes the value, completion, and successor-state outputs and derives a
fresh frontier. On failure, it emits none of those outputs and admits no later
dependent operation. Root values and scope writes commit in deterministic source
order, but commit order is not used to justify early side effects.

## Validation and observability

Graph validation checks node roles, port direction, one producer per output,
exact value/completion shape, resource version monotonicity, completion-backed
preserved sequence, and executable acyclicity. `olangc --target dot` renders
ordinary, resource, actor, completion/control, executable, and constraint nodes
with distinct styles and directed ports.

The integration suite runs graph and serial execution in isolated working
directories and compares exit status, stdout, normalized stderr, final values,
persistent Python and SQL state, environment mutation/read behavior, and full
filesystem snapshots. A test-only rendezvous also proves real worker overlap for
safe renderer tasks.

## Deliberately deferred optimization

The current resource transition model serializes read/read access. The next safe
optimization is a read-lease model in which a writer consumes every outstanding
reader-completion token. Broader parallelism additionally requires verified
backend-specific resource models. The runtime does not claim complete static
effect inference, automatic path extraction from arbitrary hosted source, or
safe arbitrary cross-runtime parallelism.
