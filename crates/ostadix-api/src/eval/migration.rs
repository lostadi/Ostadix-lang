//! Executable local handoff of settled, portable backend actors.
//!
//! These APIs consume the same process-local admission handles as evaluator
//! dispatch. They neither issue Hosted recovery warrants nor change a Hosted
//! session's placement. Only backend-owned checkpoint state moves: coordinator
//! bindings, in-flight work, escaped processes, and external resources do not.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{Evaluator, PreparedPlacementFragmentV2};
use crate::backend_state::{
    ensure_evaluator_snapshot_bound, BackendRestoreReceiptV1, EvaluatorActorCheckpointV1,
    EvaluatorStateSnapshotV1,
};
use crate::capability::BackendSandboxPolicy;

/// Receipts actually acknowledged by all newly restored backend processes.
/// This descriptive result cannot be reused as execution authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRestoreReceiptV1 {
    pub schema: String,
    pub snapshot_sha256: String,
    pub destination_evaluator: String,
    pub backend_receipts: Vec<BackendRestoreReceiptV1>,
}

/// Ownership has transferred once this result is returned. Any physical
/// shutdown failures are retained explicitly; source identities remain fenced
/// even when a source process did not acknowledge shutdown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorMigrationReceiptV1 {
    pub schema: String,
    pub source_evaluator: String,
    pub restore: ActorRestoreReceiptV1,
    pub source_shutdown_failures: Vec<String>,
}

/// Restore every actor atomically with respect to evaluator visibility. Each
/// target must be an exact destination-owned prepared persistent fragment.
/// Its code is not executed: its retained admission authorizes the matching
/// runtime, sandbox, and launch generation for the RestoreV1 handshake.
///
/// On failure, all actors created by this call are retired and all its pending
/// restores are removed; unrelated destination actors remain intact. The
/// caller retains the checkpoint and may admit fresh targets for a retry.
/// This is local recovery; remote Hosted recovery still requires its warrant.
pub fn restore_persistent_actors(
    destination: &mut Evaluator,
    snapshot: &EvaluatorStateSnapshotV1,
    targets: Vec<PreparedPlacementFragmentV2>,
    max_total_bytes: u64,
) -> Result<ActorRestoreReceiptV1> {
    ensure_evaluator_snapshot_bound(snapshot, max_total_bytes)?;
    if snapshot.actors.is_empty() {
        bail!("state.restore-empty: no persistent actors were supplied");
    }
    if targets.len() != snapshot.actors.len() {
        bail!("state.restore-targets: every checkpoint actor needs exactly one admitted target");
    }

    let mut targets_by_actor = BTreeMap::new();
    for target in targets {
        let bindings = target.bindings();
        if !bindings.environment().is_persistent() {
            bail!("state.restore-targets: an ephemeral fragment cannot receive persistent state");
        }
        let key = (
            bindings.canonical_backend().to_string(),
            bindings.environment().runtime_env_id(),
            bindings.sandbox_policy_sha256().as_sha256().to_string(),
        );
        if targets_by_actor.insert(key, target).is_some() {
            bail!("state.restore-targets: duplicate admitted actor target");
        }
    }
    let ordered_targets = snapshot
        .actors
        .iter()
        .map(|actor| {
            let target = targets_by_actor.remove(&actor_key(actor)).context(
                "state.restore-targets: backend/environment/sandbox has no admitted target",
            )?;
            validate_target(destination, actor, target)
        })
        .collect::<Result<Vec<_>>>()?;
    let snapshot_sha256 = snapshot.snapshot_sha256()?;
    destination.stage_persistent_actor_restore(snapshot.clone(), max_total_bytes)?;

    let restored: Result<Vec<BackendRestoreReceiptV1>> = (|| {
        let mut receipts = Vec::with_capacity(snapshot.actors.len());
        for (actor, target) in snapshot.actors.iter().zip(ordered_targets) {
            // Recheck retained admission and open executable identities at
            // each launch, including after earlier actors took time to ACK.
            let target = validate_target(destination, actor, target)?;
            let admitted = target.admission.bind(&target.program, &target.plan)?;
            let leases = admitted.executable_leases()?;
            let sandbox = BackendSandboxPolicy::new(actor.sandbox_permissions.iter().copied());
            let shim = destination.shim_path(&actor.canonical_backend);
            let receipt = destination
                .apply_pending_actor_restore(
                    &actor.canonical_backend,
                    actor.environment_id,
                    &sandbox,
                    &shim,
                    &leases,
                    &actor.launch_generation_sha256,
                )?
                .context("state.restore-missing: admitted actor did not acknowledge restoration")?;
            receipts.push(receipt);
        }
        Ok(receipts)
    })();

    match restored {
        Ok(backend_receipts) => Ok(ActorRestoreReceiptV1 {
            schema: "ostadix.local-actor-restore-receipt/v1".to_string(),
            snapshot_sha256,
            destination_evaluator: destination.registry.migration_origin().to_string(),
            backend_receipts,
        }),
        Err(error) => {
            for actor in &snapshot.actors {
                destination
                    .pending_backend_restores
                    .remove(&actor_key(actor));
            }
            let failures = destination
                .registry
                .retire_migration_actors(&snapshot.actors, false);
            if failures.is_empty() {
                Err(error).context("state.restore-aborted: destination actors rolled back")
            } else {
                Err(error).context(format!(
                    "state.restore-aborted: destination actors removed; physical rollback failures: {}",
                    failures.join(" | ")
                ))
            }
        }
    }
}

/// Move all settled backend actors between two exclusively borrowed local
/// evaluators. Destination restore failure leaves the live source unchanged.
/// Only after every destination ACK does the source registry fence all moved
/// identities and retire their processes. Subsequent dispatch or restoration
/// through the old evaluator cannot recreate a moved logical actor.
pub fn migrate_persistent_actors(
    source: &mut Evaluator,
    destination: &mut Evaluator,
    targets: Vec<PreparedPlacementFragmentV2>,
    max_total_bytes: u64,
) -> Result<ActorMigrationReceiptV1> {
    if !source.pending_backend_restores.is_empty() {
        bail!("state.migration-pending: source has unrestored actors");
    }
    let snapshot = source.checkpoint_persistent_actors(max_total_bytes)?;
    let restore = restore_persistent_actors(destination, &snapshot, targets, max_total_bytes)?;
    // Both evaluators remain exclusively borrowed through this commit. No
    // caller can dispatch restored destination state before source fencing.
    let source_shutdown_failures = source
        .registry
        .retire_migration_actors(&snapshot.actors, true);
    Ok(ActorMigrationReceiptV1 {
        schema: "ostadix.local-actor-migration-receipt/v1".to_string(),
        source_evaluator: source.registry.migration_origin().to_string(),
        restore,
        source_shutdown_failures,
    })
}

fn actor_key(actor: &EvaluatorActorCheckpointV1) -> (String, u32, String) {
    (
        actor.canonical_backend.clone(),
        actor.environment_id,
        actor.sandbox_policy_sha256.clone(),
    )
}

fn validate_target(
    destination: &Evaluator,
    actor: &EvaluatorActorCheckpointV1,
    mut target: PreparedPlacementFragmentV2,
) -> Result<PreparedPlacementFragmentV2> {
    if target.evaluator_instance_binding != destination.default_backend_authority {
        bail!("state.restore-origin: admitted target belongs to a different Evaluator instance");
    }
    let bindings = &target.bindings;
    if actor.canonical_backend != bindings.canonical_backend()
        || actor.environment_id != bindings.environment().runtime_env_id()
        || actor.sandbox_permissions != bindings.sandbox_permissions()
        || actor.sandbox_policy_sha256 != bindings.sandbox_policy_sha256().as_sha256()
    {
        bail!(
            "state.restore-targets: admitted actor identity or sandbox does not match checkpoint"
        );
    }
    let admitted = target.admission.bind(&target.program, &target.plan)?;
    destination.verify_admitted_runtime_context(&admitted)?;
    let leases = admitted.executable_leases()?;
    leases.verify_backend(&actor.canonical_backend)?;
    let manifest: serde_json::Value =
        serde_json::from_str(&leases.backend_manifest_json(&actor.canonical_backend)?)?;
    if manifest.get("sha256").and_then(serde_json::Value::as_str)
        != Some(actor.runtime_binding_sha256.as_str())
    {
        bail!(
            "state.restore-runtime-mismatch: target executable set differs from source checkpoint"
        );
    }
    let generation = admitted.backend_launch_generation_sha256(&actor.canonical_backend)?;
    if generation != actor.launch_generation_sha256
        || generation != bindings.backend_launch_generation().as_sha256()
    {
        bail!(
            "state.restore-generation-mismatch: target launch generation differs from source actor"
        );
    }
    destination
        .resolve_default_backend_authority(&actor.canonical_backend, &actor.sandbox_permissions)?;
    target.admission = admitted.into_prepared_parts();
    Ok(target)
}
