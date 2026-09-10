# Executable backend crossing contract V1

`O --morphism-contract python-plain-data-lossless program.O backends` enables
an explicit contract for every foreign dispatch in that evaluator. Embedders
use `Evaluator::with_morphism_contract(BackendCrossingContractV1::PythonPlainDataLossless)`.
The option applies to graph and serial execution, persistent and ephemeral
actors, autonomous workers, forced requests, and recursive `O.eval` calls.
Nested fresh callbacks park the exact outer process until its callback returns;
each nested fresh invocation receives a distinct process and session identity.
Parked processes count toward total and per-backend session limits and are
retired on failure, explicit shutdown, or callback unwinding. Persistent actor
reentry retains its existing refusal rule.
The selected contract is part of the admitted runtime context. Ordinary
execution retains its existing supported values and backend behavior.

The carrier is null, exact booleans, arbitrary integers, finite f64 values
with their bits preserved, UTF-8 text, lists, and string-keyed maps. Containers
must be finite trees of those values, with depth at most 64. The observation
retains the complete OValue in that carrier. It does not include Python object
identity, environment state, arbitrary future callbacks on a native object,
or other contextual observations.

The registry checks every binding against the lossless profile before starting
the operation. Source splices undergo the same check before rendering. The
Python adapter accepts a distinct `exec_morphism_v1` command, validates the
actual converted input, and compares its fresh OValue witness with the incoming
binding before changing actor bindings or executing user code. It snapshots
that input witness before the program can mutate its arguments.

After execution, the adapter inspects the actual native result before ordinary
lifting. Only exact built-in types in the declared carrier pass. Shared or
cyclic containers, subclasses, tuples, non-string map keys, nonfinite floats,
and opaque native objects are rejected. The result travels in a
`morphism_result_v1` receipt containing the contract, fresh invocation id,
input witnesses, and lifted output. The registry independently validates the
receipt and profile; missing, substituted, or unsolicited receipts cannot
publish a result. Other adapters reject the dedicated command instead of
silently ignoring it. Unprofiled foreign dispatch is rejected only when this
explicit mode is active.

This is an adapter conversion contract, not a statement that arbitrary code
returns its input unchanged. Program transformations are allowed. Input
rejection occurs before that operation's source runs; output rejection occurs
after the source ran and does not roll back its effects. A rejected output is
not published to successor operations. Already started autonomous operations
retain the normal documented failure and effect semantics.

Authority relies on the admitted, digest-bound adapter implementation and
launch context. Receipt matching is not authentication of a malicious runtime,
and does not protect against user code deliberately compromising its own
interpreter or wire channel. This implementation does not establish lossless
exchange of arbitrary native objects across arbitrary foreign runtimes or a
universal contextual equivalence theorem.

Validation is in `tests/backend_morphism_enforcement.rs` (real CLI, both
executors, effects, callback/deferred propagation, worker overlap, invalid
receipts) and `tests/test_morphism_enforcement_protocol.py` (actual native
input/output checks and unchanged actor bindings after input rejection).
