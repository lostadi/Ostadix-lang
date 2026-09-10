# Automatic physical execution of local OIR graphs

`computation::execute_oir_physical_v1` accepts an ordinary `OIrProgram`, an
evaluator, lexical scope, policy, and one transport implementation. Parsing and
lowering remain the normal `Parser::parse` and `OIrProgram::lower` operations.
The caller supplies no operation contracts, realization offers, or edge
records. The API establishes fresh runtime evidence and admission before
projecting the complete executable HGraph.

```rust,ignore
let nodes = Parser::new(source, &registered_backends).parse()?;
let program = OIrProgram::lower(&nodes);
let report = execute_oir_physical_v1(
    &mut evaluator,
    &program,
    &mut lexical_scope,
    Policy::Eager,
    Arc::new(LocalSocketTransportV1::default()),
)?;
```

Each `ReadySchedule` operation becomes its own physical operation. Every
producer-backed input becomes an explicit transfer task, including ordinary
values and resource, actor, completion, and control tokens. Initially
materialized inputs and ambient lexical scope become external ports. The
existing graph planner validates and selects the generated local realization
records within the planner's existing size bounds. Empty and literal-only
programs use ordinary admitted coordinator materialization and root commit;
their report has `plan: None` and no operation or transfer tasks. They never
invoke the transport. This local profile does not discover remote targets or route a whole
program as a single physical operation. An already selected remote provider
is rejected before admission rather than silently replaced by local execution.

The coordinator executes the resulting operation and transfer dependency
graph using its existing admitted adapters. Before preparing an operation,
it transports every input and its ambient scope. `LocalSocketTransportV1`
writes the actual serialized payload into a Unix socket, and a separate
receiving thread reads it with a timeout. The consumer uses the decoded
values. Source values and scope are restored after preparation; root scope
commits occur only after successful execution, in semantic root order.

Before a transfer starts, the bridge freezes a digest of the canonical OValue
or graph-bound control token. It compares the received observation and exact
typed carrier structure with the source;
a transport implementation cannot approve its own corrupted output. All
inputs are staged before the consumer frame changes. A changed payload,
scope binding, token, or container order rejects the consumer. Structural
comparison preserves duplicate map entries, set carrier order, decimal
coefficient/exponent, and raw float bits including NaN payloads. Canonical
identity remains a separate observation and never authorizes its normalized
container order to replace the original representation.
This guarantees identity of the transported carrier, including retained owner
references; it does not reconstruct a foreign native object in a new runtime.

Backend dispatch still checks the admitted runtime and live executable
authority. State, resource, actor, and control dependencies remain in the
HGraph. Explicit autonomous regions retain their bounded worker refill and
scoped provisional publication; persistent actors and ordinary fallible work
retain their ordering. Unknown effects are not asserted to commute. Callbacks
that parse additional programs retain their existing evaluator admission path;
they do not implicitly become nodes of the enclosing static physical plan.

`report.plan`, when present, exposes the generated physical tasks; `report.tasks` records
operation and planned-transfer transitions. `report.transfers` includes both
planned edges and external inputs, with consumer, port, byte count, and checked
observation. Successful reports allow every transfer-before-consumer ordering
to be checked. Runtime errors remain errors and do not return a success report.

There is one bound local realization per operation. A cost profile records
one actual local interface-serialization timing sample. Backend execution and
transfer costs remain unmeasured, so its partial total makes no comparative
performance claim. Target records describe local coordinates; they provide
no remote authentication or admission authority.

`tests/oir_physical_execution.rs` exercises a parsed Python
producer/store/load/consumer pipeline, actual transferred values, complete
task ordering, corrupt payload and scope rejection, overlapping autonomous
blocks with scope inputs, and ordinary failure before a later file effect.
