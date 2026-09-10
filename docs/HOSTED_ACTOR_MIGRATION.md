# Hosted actor migration

Hosted V2 transfers supported, idle actor state between two authenticated nodes through `HostedNodeClientV2::migrate_session`. Both endpoints must already be admitted `CheckpointRestore` sessions with established actor generations. The implementation uses real evaluator checkpoints and backend restore acknowledgements, signed node journals, and the existing placement authority. It does not replay accepted operations to reconstruct state.

The current checkpoint codecs cover the supported Python globals graph and SQLite main database state. Live native handles, in-flight evaluator work, open external resources, unsupported checkpoint values, and runtime implementation changes are not transferable by this protocol. A checkpoint refusal preserves the source. The destination initially needs its own settled checkpoint, retained for rollback if installation fails or the source cancels before fencing.

## Exact authority and identity

`MigrationPlanV2` binds a transaction identifier, both state-session identities, node signing keys, client principal digests, initial actor generations, initial journal heads, and the source checkpoint digest. Both sessions must belong to the same authenticated principal and have equal logical-environment, backend-implementation, sandbox-policy, and launch-context identities. Node and session identities remain distinct.

Each phase uses a fresh `MigrationWarrantV2` and signed placement lease with purpose `Migrate`. The warrant binds the plan, action, current local journal head, and any required peer receipt digest. The existing state-control authority verifies the exact established target, backend, sandbox, session, principal, actor generation, reservation, command digest, nonce, and bounded validity. Authentication and capability possession alone do not authorize migration. Restoration is authorized again after the backend responds; an expired or changed authorization cannot publish a successful acknowledgement.

## Transfer phases

| Action | Node | Required evidence | Durable result |
| --- | --- | --- | --- |
| `Prepare` | Source | Exact settled checkpoint and idle actor | A fresh checkpoint is validated and stored; signed `Prepared` freezes execution and exports the snapshot. |
| `Install` | Destination | Source `Prepared` receipt and matching snapshot | `Installing` consumes a new physical generation before restore; a real backend restore ACK produces signed `Installed`. The restored actor remains a standby. |
| `Fence` | Source | Exact destination `Installed` receipt referencing this preparation | Signed `Fenced` permanently changes the source to `Migrated` before actor teardown. |
| `Activate` | Destination | Exact source `Fenced` receipt referencing this installation | The acknowledged standby becomes `Ready`. If restart lost the standby, durable `Activating` allocates a new generation and requires a fresh backend restore ACK first. |
| `Abort` | Source | This source is still `Prepared` | Signed `Aborted` resumes the original live actor, or requires the existing recovery warrant if restart lost that actor. Fencing is no longer admissible. |
| `Cancel` | Destination | Exact signed source `Aborted` receipt | Signed `Cancelled` discards standby ownership, restores the previous destination checkpoint pointer, and requires recovery or close. Activation is no longer admissible. |

The destination cannot execute while installing or standing by. The source cannot execute, reset, or recover after fencing. An authenticated close of the terminal migrated source releases its session reservation while retaining the existing closed-session tombstone. An aborted destination can likewise be closed to release capacity, or recovered from its previous checkpoint under the normal exact recovery warrant.

## Durability, retries, and recovery

Snapshot storage precedes a journal reference. The existing durable quota checks reserve terminal journal headroom before allocating a restore attempt. Signed journal transitions record request and warrant digests, placement lease identity and nonce, the attempted generation, peer acknowledgement identity, and rollback checkpoint metadata. Successful replies follow the durable terminal append.

An exact duplicate request returns the original signed terminal receipt rather than repeating physical restoration or advancing ownership again. A request with stale journal evidence, altered scope, reused authority, a conflicting client sequence, an invalid peer signature, or a failed restore receipt cannot advance the transfer.

On node restart, `Prepared`, `Installed`, and `Fenced` retain their execution restrictions. An interrupted `Installing` becomes signed `InstallFailed`; its previous destination checkpoint remains available. An interrupted `Activating` becomes signed `ActivationFailed`; the source remains permanently fenced and the destination remains blocked. A newly authorized `Activate` can retry restoration using a new physical generation and the same exact source fence. Restart never invents a restore acknowledgement and never automatically replays a user operation.

The protocol chooses exclusive ownership over automatic failover during a network partition: a destination without signed source fencing stays a standby. If the source has durably fenced, it cannot be resumed merely because the destination or client is unreachable. Completion requires the retained signed evidence and an available authorized destination.

## Executable evidence

`tests/hosted_actor_migration.rs` starts two real `o-node` processes with distinct node signing keys, mutual TLS, a pinned placement authority, and actual Python or SQLite shims. It exercises state preservation and Python alias identity, phase retries, node restarts, permanent source fencing, actual backend restore refusal, exact-warrant recovery of the destination's previous state, signed standby cancellation, reservation release, and process termination while a physical restore is in progress. The tests use an instrumented fixture shim only to make the restore interruption deterministic; both nodes and the authority bind the same fixture implementation.

Run the focused suite from an isolated build checkout:

```sh
cargo test --locked --all-features --test hosted_actor_migration
```

These are separate local TLS node processes. They demonstrate the network protocol and process crash behavior, not a cross-host network qualification, operating-system memory migration, or migration of arbitrary native resources.
