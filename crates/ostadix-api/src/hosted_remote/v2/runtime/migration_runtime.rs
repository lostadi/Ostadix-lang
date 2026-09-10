//! Journaled checkpoint handoff. This is a child of the runtime so every
//! transition uses the same session mutex, principal check and fsynced store.
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::backend_state::EvaluatorStateSnapshotV1;
use crate::hosted_remote::protocol::{canonical_hosted_sha256, unix_time_ms};
use crate::hosted_remote::v2::auth::{AuthorizedPlacementV2, PlacementAuthorizationContextV2};
use crate::hosted_remote::v2::migration_protocol::{
    migration_state_from_receipt, MigrateSessionRequestV2, MigrationActionV2,
    MigrationCheckpointV2, MigrationEndpointV2, MigrationPhaseV2, MigrationPlanV2,
    MigrationStateV2, MigrationTransitionV2,
};
use crate::hosted_remote::v2::protocol::{
    validate_client_mutation_v2, validate_sha256_v2, HostedProtocolErrorV2, HostedResponseV2,
    JournalEventV2, OperationStatusV2, PlacementPurposeV2, SessionStateTierV2, SessionStatusV2,
    SignedJournalEntryV2,
};
use crate::placement::ActorGenerationIdV1;

use super::{
    apply_receipt_head, authenticate_locked, bounded_durable_text, clear_preparation,
    duplicate_commit, ensure_session_durable_capacity, record_commit, recovery_probes,
    require_next_sequence, spawn_actor, successor_actor_generation,
    validate_checkpoint_state_contract, ActorCommandV2, DurableCheckpointV2, HostedV2Runtime,
    PreparationReservationV2, RuntimeStateV2, RuntimeStoreV2, SessionRecordV2,
    RECOVERY_TERMINAL_HEADROOM_RESERVATION,
};

impl HostedV2Runtime {
    pub fn migrate_session(
        &self,
        principal: &str,
        request: MigrateSessionRequestV2,
    ) -> Result<HostedResponseV2> {
        let _call = self.inner.begin_call()?;
        self.require_store_current()?;
        request.validate()?;
        let request_sha256 = canonical_hosted_sha256(&request)?;
        let session_id = &request.credentials.session_id;
        let (preparation, mut context) = {
            let mut state = self.lock_state()?;
            authenticate_locked(&state, principal, &request.credentials)?;
            if let Some(receipt) = duplicate_commit(
                &state,
                session_id,
                request.client_sequence,
                &request.client_request_id,
                &request_sha256,
            )? {
                return migration_response(&self.inner.store, receipt, request.warrant.action);
            }
            require_next_sequence(&state, session_id, request.client_sequence)?;
            let session = &state.sessions[session_id];
            validate_migration_request(session, &request)?;
            let local_key = self
                .inner
                .store
                .with_store(|store| Ok(store.signer().public_key_hex()))?;
            let endpoint = local_endpoint(&request.warrant.plan, request.warrant.action);
            if endpoint.node_public_key != local_key {
                bail!("migration local signing key differs from authority-bound endpoint");
            }
            if session.preparation.is_some() || session.recovery_attempt.is_some() {
                bail!("migration session has an admission or recovery handshake in progress");
            }
            let preparation = PreparationReservationV2 {
                request_sha256: request_sha256.clone(),
                client_sequence: request.client_sequence,
                client_request_id: request.client_request_id.clone(),
                operation_id: request.warrant.plan.transaction_id.clone(),
                journal_head_sha256: session.journal_head_sha256.clone(),
            };
            let context = PlacementAuthorizationContextV2 {
                node_id: self.inner.config.node_id.clone(),
                node_generation: self.inner.config.node_generation,
                principal_sha256: principal.to_owned(),
                state_session: session.state_session.clone(),
                session_state_tier: session.state_tier,
                client_request_id: request.client_request_id.clone(),
                client_sequence: request.client_sequence,
                purpose: PlacementPurposeV2::Migrate,
                operation_sha256: None,
                recovery_warrant_sha256: Some(request.warrant.sha256()?),
                state_quota_generation: session.state_quota_generation,
                state_quota_limits: session.state_quota_limits.clone(),
                state_reservation: session.state_reservation.clone(),
                current_actor_generation: session.actor_generation.clone(),
                next_actor_generation: session.next_actor_generation,
                prepared_fragment: None,
                expected_session_identity: Some(session.placement_identity.clone()),
                now_unix_ms: 0,
            };
            state.sessions.get_mut(session_id).unwrap().preparation = Some(preparation.clone());
            (preparation, context)
        };
        let result = (|| {
            context.now_unix_ms = unix_time_ms()?;
            let authorized = self
                .inner
                .authorizer
                .authorize(&context, &request.placement_lease)?;
            let mut state = self.lock_state()?;
            migration_unchanged(&state, principal, &request, &preparation)?;
            if state.used_lease_nonces.contains(&authorized.lease_nonce) {
                bail!("migration placement lease nonce was already consumed");
            }
            if unix_time_ms()? >= authorized.expires_at_unix_ms {
                bail!("migration authority expired before action");
            }
            let limit = state.sessions[session_id]
                .state_reservation
                .snapshot_bytes_per_actor();
            match request.warrant.action {
                MigrationActionV2::Prepare => {
                    let worker = state.workers.get(session_id).cloned().context(
                        "migration source has no live worker; recover its checkpoint first",
                    )?;
                    let (reply, result) = mpsc::channel();
                    worker
                        .send(ActorCommandV2::CheckpointForMigration { limit, reply })
                        .map_err(|_| {
                            anyhow::anyhow!("migration source checkpoint worker disconnected")
                        })?;
                    drop(state);
                    let remaining = authorized
                        .expires_at_unix_ms
                        .saturating_sub(unix_time_ms()?);
                    let snapshot = result
                        .recv_timeout(Duration::from_millis(remaining))
                        .context("migration source checkpoint acknowledgement timed out")?
                        .map_err(anyhow::Error::msg)?;
                    context.now_unix_ms = unix_time_ms()?;
                    let fresh = self
                        .inner
                        .authorizer
                        .authorize(&context, &request.placement_lease)?;
                    if fresh != authorized {
                        bail!("migration source authority changed during checkpoint");
                    }
                    let mut state = self.lock_state()?;
                    migration_unchanged(&state, principal, &request, &preparation)?;
                    require_migration_live_lease(&authorized)?;
                    validate_checkpoint_state_contract(&snapshot, &state.sessions[session_id])?;
                    if snapshot.snapshot_sha256()? != request.warrant.plan.checkpoint_sha256 {
                        bail!("fresh migration checkpoint differs from authority-bound settled checkpoint");
                    }
                    let transition = migration_transition(
                        &request,
                        &request_sha256,
                        &authorized,
                        MigrationPhaseV2::Prepared,
                        request.warrant.plan.source.actor_generation.clone(),
                        snapshot.encoded_len()? as u64,
                        true,
                        state.sessions[session_id].actor_id.clone(),
                    )?;
                    let receipt = self.append_migration_locked(
                        &mut state,
                        session_id,
                        transition,
                        Some(&snapshot),
                    )?;
                    Ok(HostedResponseV2::Migration {
                        receipt,
                        snapshot: Some(snapshot),
                    })
                }
                MigrationActionV2::Install => {
                    let snapshot = request.snapshot.as_ref().unwrap().clone();
                    validate_checkpoint_state_contract(&snapshot, &state.sessions[session_id])?;
                    if snapshot.encoded_len()? as u64 > limit {
                        bail!("migration checkpoint exceeds destination reservation");
                    }
                    let generation = successor_actor_generation(
                        &request.warrant.plan.destination.actor_generation,
                    )?;
                    let transition = migration_transition(
                        &request,
                        &request_sha256,
                        &authorized,
                        MigrationPhaseV2::Installing,
                        generation.clone(),
                        snapshot.encoded_len()? as u64,
                        false,
                        None,
                    )?;
                    self.append_migration_locked(
                        &mut state,
                        session_id,
                        transition,
                        Some(&snapshot),
                    )?;
                    if let Some(previous) = state.workers.remove(session_id) {
                        previous.request_close();
                    }
                    drop(state);
                    self.finish_migration_restore(
                        principal,
                        &request,
                        &request_sha256,
                        &authorized,
                        &context,
                        snapshot,
                        generation,
                        MigrationPhaseV2::Installed,
                        MigrationPhaseV2::InstallFailed,
                    )
                }
                MigrationActionV2::Fence => {
                    let previous = state.sessions[session_id]
                        .migration
                        .as_ref()
                        .unwrap()
                        .state
                        .clone();
                    let transition = migration_transition(
                        &request,
                        &request_sha256,
                        &authorized,
                        MigrationPhaseV2::Fenced,
                        previous.actor_generation,
                        previous.checkpoint_bytes,
                        true,
                        None,
                    )?;
                    let receipt =
                        self.append_migration_locked(&mut state, session_id, transition, None)?;
                    // The signed durable fence is authoritative even if the
                    // process exits before this best-effort worker teardown.
                    if let Some(worker) = state.workers.remove(session_id) {
                        worker.request_close();
                    }
                    Ok(HostedResponseV2::Migration {
                        receipt,
                        snapshot: None,
                    })
                }
                MigrationActionV2::Activate => {
                    let previous = state.sessions[session_id]
                        .migration
                        .as_ref()
                        .unwrap()
                        .state
                        .clone();
                    if state.workers.contains_key(session_id) {
                        let transition = migration_transition(
                            &request,
                            &request_sha256,
                            &authorized,
                            MigrationPhaseV2::Activated,
                            previous.actor_generation,
                            previous.checkpoint_bytes,
                            true,
                            previous.actor_id,
                        )?;
                        let receipt =
                            self.append_migration_locked(&mut state, session_id, transition, None)?;
                        return Ok(HostedResponseV2::Migration {
                            receipt,
                            snapshot: None,
                        });
                    }
                    // A destination restart loses the standby process. Allocate
                    // a fresh generation durably and obtain another real ACK.
                    let snapshot = self.inner.store.read_checkpoint(
                        session_id,
                        &request.warrant.plan.checkpoint_sha256,
                        previous.checkpoint_bytes,
                    )?;
                    let generation = successor_actor_generation(&previous.actor_generation)?;
                    let transition = migration_transition(
                        &request,
                        &request_sha256,
                        &authorized,
                        MigrationPhaseV2::Activating,
                        generation.clone(),
                        previous.checkpoint_bytes,
                        false,
                        None,
                    )?;
                    self.append_migration_locked(&mut state, session_id, transition, None)?;
                    drop(state);
                    self.finish_migration_restore(
                        principal,
                        &request,
                        &request_sha256,
                        &authorized,
                        &context,
                        snapshot,
                        generation,
                        MigrationPhaseV2::Activated,
                        MigrationPhaseV2::ActivationFailed,
                    )
                }
                MigrationActionV2::Abort => {
                    let previous = state.sessions[session_id]
                        .migration
                        .as_ref()
                        .unwrap()
                        .state
                        .clone();
                    let actor_id = state
                        .workers
                        .contains_key(session_id)
                        .then(|| state.sessions[session_id].actor_id.clone())
                        .flatten();
                    let transition = migration_transition(
                        &request,
                        &request_sha256,
                        &authorized,
                        MigrationPhaseV2::Aborted,
                        previous.actor_generation,
                        previous.checkpoint_bytes,
                        true,
                        actor_id,
                    )?;
                    let receipt =
                        self.append_migration_locked(&mut state, session_id, transition, None)?;
                    Ok(HostedResponseV2::Migration {
                        receipt,
                        snapshot: None,
                    })
                }
                MigrationActionV2::Cancel => {
                    let previous = state.sessions[session_id]
                        .migration
                        .as_ref()
                        .unwrap()
                        .state
                        .clone();
                    let transition = migration_transition(
                        &request,
                        &request_sha256,
                        &authorized,
                        MigrationPhaseV2::Cancelled,
                        previous.actor_generation,
                        previous.checkpoint_bytes,
                        true,
                        None,
                    )?;
                    let receipt =
                        self.append_migration_locked(&mut state, session_id, transition, None)?;
                    if let Some(worker) = state.workers.remove(session_id) {
                        worker.request_close();
                    }
                    Ok(HostedResponseV2::Migration {
                        receipt,
                        snapshot: None,
                    })
                }
            }
        })();
        // Terminal transitions clear this token themselves. Pre-commit errors
        // restore ordinary admission; a started restore remains journal-fenced.
        clear_preparation(&self.inner, session_id, &preparation)?;
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_migration_restore(
        &self,
        principal: &str,
        request: &MigrateSessionRequestV2,
        request_sha256: &str,
        authorized: &AuthorizedPlacementV2,
        original_context: &PlacementAuthorizationContextV2,
        snapshot: EvaluatorStateSnapshotV1,
        generation: ActorGenerationIdV1,
        success: MigrationPhaseV2,
        failure: MigrationPhaseV2,
    ) -> Result<HostedResponseV2> {
        let session_id = &request.credentials.session_id;
        let restore = (|| {
            require_migration_live_lease(authorized)?;
            let probes = recovery_probes(
                session_id,
                &request.client_request_id,
                &snapshot,
                &generation,
            )?;
            let worker = spawn_actor(
                &self.inner,
                session_id,
                SessionStateTierV2::CheckpointRestore,
            )?;
            let deadline = Instant::now()
                + Duration::from_millis(
                    authorized
                        .expires_at_unix_ms
                        .saturating_sub(unix_time_ms()?),
                );
            let (reply, acknowledgement) = mpsc::channel();
            if worker
                .send(ActorCommandV2::Recover {
                    snapshot: snapshot.clone(),
                    snapshot_limit: original_context
                        .state_reservation
                        .snapshot_bytes_per_actor(),
                    probes,
                    deadline,
                    reply,
                })
                .is_err()
            {
                worker.request_close();
                bail!("migration restore worker disconnected");
            }
            match acknowledgement.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Ok(())) => {}
                other => {
                    worker.request_close();
                    bail!("migration backend restore did not acknowledge: {other:?}");
                }
            }
            let mut context = original_context.clone();
            context.now_unix_ms = unix_time_ms()?;
            match self
                .inner
                .authorizer
                .authorize(&context, &request.placement_lease)
            {
                Ok(fresh) if &fresh == authorized => {}
                other => {
                    worker.request_close();
                    bail!("migration authority changed before restore commit: {other:?}");
                }
            }
            if let Err(error) = require_migration_live_lease(authorized) {
                worker.request_close();
                return Err(error);
            }
            Ok(worker)
        })();
        let mut state = self.lock_state()?;
        authenticate_locked(&state, principal, &request.credentials)?;
        let pending = state.sessions[session_id]
            .migration
            .as_ref()
            .context("migration restore allocation disappeared")?;
        if pending.request_sha256 != request_sha256
            || pending.terminal
            || pending.state.actor_generation != generation
        {
            if let Ok(worker) = restore {
                worker.request_close();
            }
            bail!("migration restore allocation changed before acknowledgement");
        }
        let actor_id = restore.as_ref().ok().map(|_| {
            format!(
                "migration:{}:{}",
                request.warrant.plan.transaction_id,
                generation.generation().get()
            )
        });
        let phase = if restore.is_ok() { success } else { failure };
        let mut transition = migration_transition(
            request,
            request_sha256,
            authorized,
            phase,
            generation,
            snapshot.encoded_len()? as u64,
            true,
            actor_id,
        )?;
        transition.state.failure = restore.as_ref().err().map(|error| {
            HostedProtocolErrorV2::new(
                "migration-restore-failed",
                bounded_durable_text(&format!("{error:#}")),
                false,
            )
        });
        let receipt = match self.append_migration_locked(&mut state, session_id, transition, None) {
            Ok(receipt) => receipt,
            Err(error) => {
                if let Ok(worker) = restore {
                    worker.request_close();
                }
                return Err(error);
            }
        };
        if let Ok(worker) = restore {
            state.workers.insert(session_id.clone(), worker);
        }
        Ok(HostedResponseV2::Migration {
            receipt,
            snapshot: None,
        })
    }

    fn append_migration_locked(
        &self,
        state: &mut RuntimeStateV2,
        session_id: &str,
        mut transition: MigrationTransitionV2,
        snapshot: Option<&EvaluatorStateSnapshotV1>,
    ) -> Result<SignedJournalEntryV2> {
        if transition.state.phase == MigrationPhaseV2::Installing {
            transition.state.previous_checkpoint = state.sessions[session_id]
                .checkpoint
                .as_ref()
                .map(|checkpoint| MigrationCheckpointV2 {
                    actor_generation: checkpoint.actor_generation.clone(),
                    snapshot_sha256: checkpoint.snapshot_sha256.clone(),
                    snapshot_bytes: checkpoint.snapshot_bytes,
                });
        } else if transition.state.plan.destination.state_session
            == state.sessions[session_id].state_session
        {
            transition.state.previous_checkpoint = state.sessions[session_id]
                .migration
                .as_ref()
                .and_then(|record| record.state.previous_checkpoint.clone());
        }
        validate_migration_transition(&state.sessions[session_id], &transition)?;
        if state
            .used_lease_nonces
            .contains(&transition.placement_lease_nonce)
            && !state.sessions[session_id]
                .migration
                .as_ref()
                .is_some_and(|pending| {
                    !pending.terminal
                        && pending.request_sha256 == transition.request_sha256
                        && pending.placement_lease_nonce == transition.placement_lease_nonce
                })
        {
            bail!("migration placement lease nonce was already consumed");
        }
        let receipt = self.issue_next_entry(
            &state.sessions[session_id],
            unix_time_ms()?,
            JournalEventV2::MigrationTransition {
                transition: Box::new(transition.clone()),
            },
        )?;
        let frame = self.inner.store.encoded_frame_bytes(&receipt)?;
        if frame > RECOVERY_TERMINAL_HEADROOM_RESERVATION {
            bail!("migration transition exceeds fixed terminal headroom");
        }
        let limit = state.sessions[session_id]
            .state_reservation
            .snapshot_bytes_per_actor();
        let blob = snapshot
            .map(|snapshot| {
                self.inner.store.checkpoint_new_bytes(
                    session_id,
                    &transition.state.plan.checkpoint_sha256,
                    snapshot,
                    limit,
                )
            })
            .transpose()?
            .unwrap_or(0);
        let future_terminal = if transition.terminal {
            0
        } else {
            RECOVERY_TERMINAL_HEADROOM_RESERVATION
        };
        ensure_session_durable_capacity(
            state,
            session_id,
            frame
                .checked_add(blob)
                .and_then(|n| n.checked_add(future_terminal))
                .context("migration quota overflow")?,
        )?;
        if let Some(snapshot) = snapshot {
            let written = self.inner.store.write_checkpoint(
                session_id,
                &transition.state.plan.checkpoint_sha256,
                snapshot,
                limit,
            )?;
            if written != blob {
                bail!("migration checkpoint accounting changed");
            }
            state.durable_bytes = state
                .durable_bytes
                .checked_add(written)
                .context("migration durable accounting overflow")?;
            let session = state.sessions.get_mut(session_id).unwrap();
            session.durable_bytes = session
                .durable_bytes
                .checked_add(written)
                .context("migration session accounting overflow")?;
        }
        let written = self.inner.store.append_entry(session_id, &receipt)?;
        state.durable_bytes = state
            .durable_bytes
            .checked_add(written)
            .context("migration durable accounting overflow")?;
        state
            .used_lease_nonces
            .insert(transition.placement_lease_nonce.clone());
        let session = state.sessions.get_mut(session_id).unwrap();
        session.durable_bytes = session
            .durable_bytes
            .checked_add(written)
            .context("migration session accounting overflow")?;
        apply_migration_transition(session, &transition, &receipt)?;
        apply_receipt_head(session, &receipt);
        Ok(receipt)
    }

    pub(super) fn repair_migration_restart_locked(
        &self,
        state: &mut RuntimeStateV2,
        session_id: &str,
    ) -> Result<bool> {
        let Some(previous) = state.sessions[session_id].migration.clone() else {
            return Ok(false);
        };
        if !previous.state.phase.blocks_execution() {
            return Ok(false);
        }
        if !previous.terminal {
            let mut refusal = previous;
            refusal.terminal = true;
            refusal.state.phase = match refusal.state.phase {
                MigrationPhaseV2::Installing => MigrationPhaseV2::InstallFailed,
                MigrationPhaseV2::Activating => MigrationPhaseV2::ActivationFailed,
                _ => bail!("unfinished migration has no restore attempt"),
            };
            refusal.state.actor_id = None;
            refusal.state.failure = Some(HostedProtocolErrorV2::new(
                "migration-restore-interrupted",
                "node restarted after durable restore allocation and before its acknowledgement",
                false,
            ));
            self.append_migration_locked(state, session_id, refusal, None)?;
        }
        let session = state.sessions.get_mut(session_id).unwrap();
        session.actor_id = None;
        session.actor_has_state = false;
        Ok(true)
    }
}

fn local_endpoint(plan: &MigrationPlanV2, action: MigrationActionV2) -> &MigrationEndpointV2 {
    match action {
        MigrationActionV2::Prepare | MigrationActionV2::Fence | MigrationActionV2::Abort => {
            &plan.source
        }
        _ => &plan.destination,
    }
}
fn require_migration_live_lease(authorized: &AuthorizedPlacementV2) -> Result<()> {
    if unix_time_ms()? >= authorized.expires_at_unix_ms {
        bail!("migration placement authority expired");
    }
    Ok(())
}
fn migration_unchanged(
    state: &RuntimeStateV2,
    principal: &str,
    request: &MigrateSessionRequestV2,
    reservation: &PreparationReservationV2,
) -> Result<()> {
    authenticate_locked(state, principal, &request.credentials)?;
    let session = &state.sessions[&request.credentials.session_id];
    if session.preparation.as_ref() != Some(reservation)
        || session.journal_head_sha256 != reservation.journal_head_sha256
    {
        bail!("migration admission coordinates changed");
    }
    validate_migration_request(session, request)
}
#[allow(clippy::too_many_arguments)]
fn migration_transition(
    request: &MigrateSessionRequestV2,
    request_sha256: &str,
    authorized: &AuthorizedPlacementV2,
    phase: MigrationPhaseV2,
    actor_generation: ActorGenerationIdV1,
    checkpoint_bytes: u64,
    terminal: bool,
    actor_id: Option<String>,
) -> Result<MigrationTransitionV2> {
    Ok(MigrationTransitionV2 {
        state: MigrationStateV2 {
            plan: request.warrant.plan.clone(),
            phase,
            checkpoint_bytes,
            actor_generation,
            peer_receipt_sha256: request.warrant.peer_receipt_sha256.clone(),
            actor_id,
            failure: None,
            previous_checkpoint: None,
        },
        client_sequence: request.client_sequence,
        client_request_id: request.client_request_id.clone(),
        request_sha256: request_sha256.to_owned(),
        warrant_sha256: request.warrant.sha256()?,
        placement_lease_sha256: authorized.lease_sha256.clone(),
        placement_lease_nonce: authorized.lease_nonce.clone(),
        terminal,
    })
}
fn migration_response(
    store: &RuntimeStoreV2,
    receipt: SignedJournalEntryV2,
    action: MigrationActionV2,
) -> Result<HostedResponseV2> {
    let snapshot = if action == MigrationActionV2::Prepare {
        let state = migration_state_from_receipt(&receipt)?;
        Some(store.read_checkpoint(
            &receipt.entry.session_id,
            &state.plan.checkpoint_sha256,
            state.checkpoint_bytes,
        )?)
    } else {
        None
    };
    Ok(HostedResponseV2::Migration { receipt, snapshot })
}

fn validate_migration_request(
    session: &SessionRecordV2,
    request: &MigrateSessionRequestV2,
) -> Result<()> {
    let plan = &request.warrant.plan;
    let endpoint = local_endpoint(plan, request.warrant.action);
    if endpoint.state_session != session.state_session
        || endpoint.principal_sha256 != session.principal_sha256
        || endpoint.session_id()? != session.session_id
        || session.state_tier != SessionStateTierV2::CheckpointRestore
    {
        bail!("migration endpoint does not match exact checkpoint session/principal");
    }
    if request.warrant.expected_journal_head_sha256 != session.journal_head_sha256 {
        bail!("migration warrant journal head is stale");
    }
    if session.operations.values().any(|op| {
        matches!(
            op.view.status,
            OperationStatusV2::Accepted | OperationStatusV2::Running | OperationStatusV2::Ambiguous
        )
    }) {
        bail!("migration requires a settled actor without ambiguous operations");
    }
    let previous = session.migration.as_ref().map(|record| &record.state);
    match request.warrant.action {
        MigrationActionV2::Prepare | MigrationActionV2::Install => {
            if session.status != SessionStatusV2::Ready
                || session.actor_generation.as_ref() != Some(&endpoint.actor_generation)
                || session.journal_head_sha256 != endpoint.journal_head_sha256
            {
                bail!("migration endpoint is not the exact ready actor/head admitted by the plan");
            }
            if request.warrant.action == MigrationActionV2::Prepare
                && session
                    .checkpoint
                    .as_ref()
                    .map(|checkpoint| &checkpoint.snapshot_sha256)
                    != Some(&plan.checkpoint_sha256)
            {
                bail!("migration plan does not bind source's settled checkpoint");
            }
        }
        MigrationActionV2::Fence | MigrationActionV2::Abort => {
            if !previous.is_some_and(|prior| {
                prior.plan == *plan && prior.phase == MigrationPhaseV2::Prepared
            }) || session.status != SessionStatusV2::Migrating
            {
                bail!("source is not prepared for this exact migration");
            }
        }
        MigrationActionV2::Activate | MigrationActionV2::Cancel => {
            if !previous.is_some_and(|prior| {
                prior.plan == *plan
                    && matches!(
                        prior.phase,
                        MigrationPhaseV2::Installed | MigrationPhaseV2::ActivationFailed
                    )
            }) || session.status != SessionStatusV2::Migrating
            {
                bail!("destination is not awaiting activation for this exact migration");
            }
        }
    }
    if let Some(receipt) = &request.peer_receipt {
        let peer = migration_state_from_receipt(receipt)?;
        let (expected_endpoint, phase) = match request.warrant.action {
            MigrationActionV2::Install => (&plan.source, MigrationPhaseV2::Prepared),
            MigrationActionV2::Fence => (&plan.destination, MigrationPhaseV2::Installed),
            MigrationActionV2::Activate => (&plan.source, MigrationPhaseV2::Fenced),
            MigrationActionV2::Cancel => (&plan.source, MigrationPhaseV2::Aborted),
            _ => bail!("unexpected migration peer proof"),
        };
        if receipt.signer_public_key != expected_endpoint.node_public_key
            || receipt.entry.session_id != expected_endpoint.session_id()?
            || peer.plan != *plan
            || peer.phase != phase
        {
            bail!("migration peer proof has wrong signer, session, plan or phase");
        }
        let JournalEventV2::MigrationTransition { transition } = &receipt.entry.event else {
            unreachable!()
        };
        if !transition.terminal {
            bail!("migration peer restore has not acknowledged");
        }
        if request.warrant.action == MigrationActionV2::Install {
            let snapshot = request.snapshot.as_ref().unwrap();
            if peer.actor_generation != plan.source.actor_generation
                || receipt.entry.previous_entry_sha256.as_ref()
                    != Some(&plan.source.journal_head_sha256)
            {
                bail!(
                    "source prepare receipt changes the admitted actor generation or journal head"
                );
            }
            if snapshot.encoded_len()? as u64 != peer.checkpoint_bytes {
                bail!("migration checkpoint length differs from signed source receipt");
            }
        }
        if request.warrant.action == MigrationActionV2::Fence {
            if peer.actor_generation
                != successor_actor_generation(&plan.destination.actor_generation)?
                || peer.actor_id.is_none()
            {
                bail!("destination ACK does not bind the exact restored successor generation");
            }
            let local_prepare = session.migration.as_ref().unwrap();
            if peer.peer_receipt_sha256.as_ref()
                != Some(
                    &session.commits[&local_prepare.client_sequence]
                        .receipt
                        .entry_sha256,
                )
            {
                bail!("destination ACK does not bind this exact source prepare receipt");
            }
        }
        if matches!(
            request.warrant.action,
            MigrationActionV2::Activate | MigrationActionV2::Cancel
        ) && peer.actor_generation != plan.source.actor_generation
        {
            bail!("source terminal receipt changes the authority-bound actor generation");
        }
        if request.warrant.action == MigrationActionV2::Activate {
            let installed_receipt = session.commits.values().find(|commit| matches!(&commit.receipt.entry.event,
                JournalEventV2::MigrationTransition { transition } if transition.state.plan == *plan && transition.state.phase == MigrationPhaseV2::Installed));
            if installed_receipt.is_none_or(|commit| {
                peer.peer_receipt_sha256.as_ref() != Some(&commit.receipt.entry_sha256)
            }) {
                bail!("source fence does not acknowledge this destination restore receipt");
            }
        }
    }
    Ok(())
}

pub(super) fn validate_migration_transition(
    session: &SessionRecordV2,
    transition: &MigrationTransitionV2,
) -> Result<()> {
    use MigrationPhaseV2::*;
    let current = &transition.state;
    current.plan.validate()?;
    validate_client_mutation_v2(transition.client_sequence, &transition.client_request_id)?;
    for digest in [
        &transition.request_sha256,
        &transition.warrant_sha256,
        &transition.placement_lease_sha256,
        &transition.placement_lease_nonce,
    ] {
        validate_sha256_v2("migration transition digest", digest)?;
    }
    if transition.client_sequence != session.next_client_sequence
        || current.checkpoint_bytes == 0
        || current.checkpoint_bytes > session.state_reservation.snapshot_bytes_per_actor()
    {
        bail!("migration transition violates sequence or checkpoint reservation");
    }
    let endpoint = if matches!(current.phase, Prepared | Fenced | Aborted) {
        &current.plan.source
    } else {
        &current.plan.destination
    };
    if endpoint.state_session != session.state_session
        || endpoint.principal_sha256 != session.principal_sha256
    {
        bail!("migration journal changes session authority");
    }
    let prior = session.migration.as_ref();
    let prior_state = prior.map(|record| &record.state);
    let same = prior_state.is_some_and(|previous| previous.plan == current.plan);
    let valid = match current.phase {
        Prepared => {
            session.status == SessionStatusV2::Ready
                && session.actor_generation.as_ref() == Some(&current.plan.source.actor_generation)
                && session.journal_head_sha256 == current.plan.source.journal_head_sha256
                && current.actor_generation == current.plan.source.actor_generation
        }
        Installing => {
            session.status == SessionStatusV2::Ready
                && session.actor_generation.as_ref()
                    == Some(&current.plan.destination.actor_generation)
                && session.journal_head_sha256 == current.plan.destination.journal_head_sha256
                && current.actor_generation
                    == successor_actor_generation(&current.plan.destination.actor_generation)?
        }
        Installed | InstallFailed => {
            same && prior_state.is_some_and(|s| {
                s.phase == Installing && s.actor_generation == current.actor_generation
            })
        }
        Fenced | Aborted => {
            same && prior_state.is_some_and(|s| {
                s.phase == Prepared && s.actor_generation == current.actor_generation
            })
        }
        Cancelled => {
            same && prior_state.is_some_and(|s| {
                matches!(s.phase, Installed | ActivationFailed)
                    && s.actor_generation == current.actor_generation
            })
        }
        Activating => {
            same && prior_state.is_some_and(|s| matches!(s.phase, Installed | ActivationFailed))
                && current.actor_generation
                    == successor_actor_generation(
                        session
                            .actor_generation
                            .as_ref()
                            .context("migration activation actor absent")?,
                    )?
        }
        Activated => {
            same && prior_state.is_some_and(|s| {
                matches!(s.phase, Installed | Activating)
                    && s.actor_generation == current.actor_generation
            })
        }
        ActivationFailed => {
            same && prior_state.is_some_and(|s| {
                s.phase == Activating && s.actor_generation == current.actor_generation
            })
        }
    };
    if !valid || transition.terminal == matches!(current.phase, Installing | Activating) {
        bail!("invalid migration journal phase/generation transition");
    }
    if let Some(prior) = prior.filter(|prior| !prior.terminal) {
        if prior.request_sha256 != transition.request_sha256
            || prior.client_sequence != transition.client_sequence
            || prior.client_request_id != transition.client_request_id
            || prior.warrant_sha256 != transition.warrant_sha256
            || prior.placement_lease_nonce != transition.placement_lease_nonce
            || prior.placement_lease_sha256 != transition.placement_lease_sha256
        {
            bail!("migration terminal differs from durable restore allocation");
        }
    }
    if matches!(current.phase, Installed | Activated) && current.actor_id.is_none() {
        bail!("migration acknowledgement lacks physical actor identity");
    }
    if current.failure.is_some() != matches!(current.phase, InstallFailed | ActivationFailed) {
        bail!("migration failure evidence does not match transition phase");
    }
    if endpoint.state_session == current.plan.destination.state_session {
        let rollback = current
            .previous_checkpoint
            .as_ref()
            .context("migration destination omits its previous checkpoint")?;
        validate_sha256_v2("migration previous checkpoint", &rollback.snapshot_sha256)?;
        if rollback.snapshot_bytes == 0
            || rollback.snapshot_bytes > session.state_reservation.snapshot_bytes_per_actor()
            || rollback.actor_generation != current.plan.destination.actor_generation
            || (current.phase == Installing
                && !session.checkpoint.as_ref().is_some_and(|checkpoint| {
                    checkpoint.actor_generation == rollback.actor_generation
                        && checkpoint.snapshot_sha256 == rollback.snapshot_sha256
                        && checkpoint.snapshot_bytes == rollback.snapshot_bytes
                }))
            || (current.phase != Installing
                && prior_state.and_then(|s| s.previous_checkpoint.as_ref()) != Some(rollback))
        {
            bail!("migration rollback checkpoint differs from original destination state");
        }
    }
    Ok(())
}

pub(super) fn apply_migration_transition(
    session: &mut SessionRecordV2,
    transition: &MigrationTransitionV2,
    receipt: &SignedJournalEntryV2,
) -> Result<()> {
    use MigrationPhaseV2::*;
    validate_migration_transition(session, transition)?;
    let current = &transition.state;
    session.actor_generation = Some(current.actor_generation.clone());
    session.actor_id = current.actor_id.clone();
    session.actor_has_state = matches!(current.phase, Prepared | Installed | Activated)
        || (current.phase == Aborted && current.actor_id.is_some());
    session.next_actor_generation = if matches!(
        current.phase,
        Installing | Activating | InstallFailed | ActivationFailed | Fenced | Cancelled
    ) || (current.phase == Aborted && current.actor_id.is_none())
    {
        successor_actor_generation(&current.actor_generation)?.generation()
    } else {
        current.actor_generation.generation()
    };
    session.status = match current.phase {
        Prepared | Installing | Installed | Activating | ActivationFailed => {
            SessionStatusV2::Migrating
        }
        Fenced => SessionStatusV2::Migrated,
        Activated => SessionStatusV2::Ready,
        Aborted if current.actor_id.is_some() => SessionStatusV2::Ready,
        InstallFailed | Aborted | Cancelled => SessionStatusV2::RecoveryRequired,
    };
    if matches!(current.phase, Prepared | Installed | Activated) {
        session.checkpoint = Some(DurableCheckpointV2 {
            actor_generation: current.actor_generation.clone(),
            snapshot_sha256: current.plan.checkpoint_sha256.clone(),
            snapshot_bytes: current.checkpoint_bytes,
        });
    }
    if current.phase == Cancelled {
        let rollback = current.previous_checkpoint.as_ref().unwrap();
        session.checkpoint = Some(DurableCheckpointV2 {
            actor_generation: rollback.actor_generation.clone(),
            snapshot_sha256: rollback.snapshot_sha256.clone(),
            snapshot_bytes: rollback.snapshot_bytes,
        });
    }
    session.migration = Some(transition.clone());
    if transition.terminal {
        session.preparation = None;
        record_commit(
            session,
            transition.client_sequence,
            transition.client_request_id.clone(),
            transition.request_sha256.clone(),
            receipt.clone(),
        )?;
    }
    Ok(())
}
