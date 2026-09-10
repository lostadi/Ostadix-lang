# Python native object handles

`O.native(value)` retains any Python object in its current backend process and
returns an opaque `OValue::Native` descriptor. It does not serialize the object,
run `repr`, pickle it, or reconstruct it in another runtime. Closures, arbitrary
instances, generators, mutable aliases, and cycles retain their Python identity.

```text
let handle = python[7]^(O.native(lambda value: value + 2))_python[7]
python[7]^(O.resolve_native($handle)(40))_python[7]
python[7]^(O.release_native($handle))_python[7]
```

`O.resolve_native(handle)` returns the original object in the original owner
process. Repeated resolution returns that same object. Descriptors can be
carried through O and echoed by other Python actors as opaque values; a foreign
actor cannot resolve them locally. The descriptor uses a random process origin and a
random 256-bit token, and the owner verifies the complete exported descriptor
before resolving or releasing it. Changed, unknown, released, or foreign-owner
handles fail explicitly. A forked process cannot reuse its parent's store.

Foreign code can operate on that exact retained object through the evaluator:

```text
let handle = python[7]^(O.native(lambda value: value + 2))_python[7]
javascript^(console.log(O.native_call(handle, 40)))_javascript
native_release($handle)
```

The JavaScript adapter supplies `O.native_call(handle, ...args)`,
`O.native_get(handle, name)`, `O.native_set(handle, name, value)`, and
`O.native_release(handle)` on Unix. Python supplies the same methods.
The O builtins are `native_call(handle, argument_list)`, `native_get(handle, name)`,
`native_set(handle, name, value)`, and `native_release(handle)`.
`get` and `set` access Python attributes; Python descriptors can execute user
code. Obtain a bound method with `get` and invoke that returned handle with `call`.

Every operation routes through the evaluator to the exact live physical owner,
session, and admitted launch generation. It never spawns a replacement owner.
The Python store then verifies the full descriptor seal and all argument handles
before invoking a callable, descriptor, setter, or release finalizer. Handles
passed as arguments resolve to their original objects. Plain data arguments are
explicit value copies; JavaScript rejects unsupported values and shared/cyclic
plain argument containers. Export those objects in Python and pass handles when
their identity must survive. The dedicated Node socket keeps callbacks separate
from ordinary stdout/stderr and bounds each bridge frame to 16 MiB.

Exact immutable scalar results cross as plain values (large integers become
JavaScript `BigInt` when needed). Mutable containers, arbitrary instances,
callables, and scalar values outside the supported plain carrier return a new
owner handle. For example, a method returning `self` returns another export of
the same Python object, with no serialized reconstruction. Each export must be
released separately. Native result exports share the existing handle quota;
quota failure after a method executes does not undo its effects.

Operations are effectful coordinator work, including attribute reads. They
execute under the owner's existing admitted backend authority. An exported
descriptor delegates access to that owner object within its evaluator; carrying
it to a different evaluator grants no process ownership. Operations inherit the
callback deadline (or the backend operation timeout), allow at most 64 nested
owner operations, and terminate an owner whose operation cannot settle. An
operation may call `O.eval`; its callback can perform another native operation
at that explicit suspension boundary. An ordinary reentrant `python[N]` block
remains forbidden. Handle/seal/argument failures precede owner user code;
exceptions or timeouts after admitted user code starts provide no rollback.

`O.release_native(handle)` releases one explicit export and invalidates every
copy of that descriptor. Separate calls to `O.native` allocate separate exports,
even for the same object, and each must be released. Already resolved local
Python references keep their normal Python lifetime; release does not revoke
those references. Backend cleanup invalidates all remaining exports.

Use a persistent `python[N]` owner for a handle needed by later blocks. A fresh
`python^(...)_python` actor is retired when its block finishes, so its returned
handle cannot subsequently be resolved. Owner exit has the same effect. An
attempt to resolve such a handle in a replacement actor reports
`native.owner-mismatch`; a released or cleaned-up handle in its existing owner
reports `native.handle-expired`.
Coordinator operations on a retired, foreign-evaluator, or replaced physical
owner instead report `native.owner-expired`. Active fresh owners held by the
coordinator can service operations while suspended at `O.eval`; autonomous worker
owners are not registered as coordinator-owned actors and cannot be addressed
through this route. Use a persistent owner for cross-runtime operations.

The descriptor declares `LiveHandle`, `SameProcess`, and an effectful boundary.
It is not cache-safe, replay-safe, or boot-persistable. The stronger Python plain
data morphism contract rejects these handles; it does not relabel them as
structural data. Native object methods continue to run under the existing
Python actor's execution authority.

While any exported handle remains live, Python checkpointing reports
`state.pin-required` at `$native_handles`. This prevents local actor migration
or checkpoint-based recovery from silently replacing an owner with a process
that lacks its objects. Releasing every export permits checkpointing only when
the remaining actor state is supported by the existing checkpoint codec.

Each process allows 4096 simultaneous exports by default. Set
`O_PYTHON_MAX_NATIVE_HANDLES` to a positive integer before starting the actor to
select a different bound. The bound counts handles; it does not estimate the
memory reachable through arbitrary retained objects. Capacity exhaustion leaves
existing handles valid, and release returns capacity.

`tests/test_python_native_handles.py` exercises the real shim protocol;
`tests/python_native_handles.rs` checks OIR/wire round trips, owner expiry,
checkpoint pinning, and migration after release.
