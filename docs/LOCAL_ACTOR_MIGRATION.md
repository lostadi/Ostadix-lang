# Local actor recovery and migration

`ostadix_api::eval::migration` executes checkpoint restoration and ownership
handoff between local evaluators. It uses the existing backend-state protocol
and consumes destination-owned `PreparedPlacementFragmentV2` admission handles.

`restore_persistent_actors(destination, snapshot, targets, max_bytes)` restores
an existing `EvaluatorStateSnapshotV1`, including after its original evaluator
has exited. Each checkpoint actor needs one admitted persistent target with the
same backend, environment, sandbox, executable-set digest, and launch generation.
The target fragment's code is not executed. Its retained runtime admission is
rechecked before sending the backend a `RestoreV1` request. The returned receipt
contains the actual matching backend acknowledgments in canonical snapshot order.

`migrate_persistent_actors(source, destination, targets, max_bytes)` first
checkpoints every settled persistent source actor. After every destination
backend acknowledges restoration, it fences the moved identities in the source
registry and retires the source processes. Both evaluators are exclusively
borrowed throughout this sequence: callers cannot dispatch restored destination
actors before source fencing completes. Subsequent execution or restore through
the old evaluator rejects a moved identity with `state.actor-migrated`.

If destination validation or restoration fails, the source remains usable.
Restored destination actors and staged checkpoints belonging to the failed
transaction are removed; unrelated destination actors remain intact. Physical
rollback failures are included in the error. After a successful transfer, physical
source shutdown failures are included in `source_shutdown_failures`; source
identities remain fenced and the destination owns the restored state.

The supported state is whatever the backend checkpoint codec can represent
without external resource bindings. Current examples include Python's constrained
object graph with aliases and cycles, and SQLite's autocommit main database.
Unsupported Python functions, open external resources, and unsupported SQLite
connection state still require pinning. These APIs do not move coordinator
bindings, in-flight operations, process memory, or external resources. The
handoff fence is local to the source evaluator and is not a durable distributed
commit protocol.

Hosted V2 recovery keeps its existing authenticated warrant and placement
boundaries. These local APIs do not issue warrants, move Hosted sessions between
nodes, or alter placement identity.

`tests/actor_migration.rs` exercises Python and SQLite processes, source liveness
at destination restore acknowledgment, source fencing, recovery after source
exit, rollback after a later actor refuses restoration, preservation of unrelated
destination state, and rejection of substituted admission/runtime identities.
