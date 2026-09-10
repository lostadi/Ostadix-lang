//! Authenticated, quiescent checkpoint transfer between two admitted sessions.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::super::protocol::canonical_hosted_sha256;
use super::protocol::*;
use crate::backend_state::EvaluatorStateSnapshotV1;
use crate::placement::{ActorGenerationIdV1, CanonicalPlacementRecordV1, StateSessionIdV2};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationEndpointV2 {
    pub state_session: StateSessionIdV2,
    pub node_public_key: String,
    pub principal_sha256: String,
    pub actor_generation: ActorGenerationIdV1,
    pub journal_head_sha256: String,
}

impl MigrationEndpointV2 {
    pub fn session_id(&self) -> Result<String> {
        Ok(self.state_session.semantic_digest()?.to_string())
    }
    pub fn validate(&self) -> Result<()> {
        validate_sha256_v2("migration node key", &self.node_public_key)?;
        validate_sha256_v2("migration principal", &self.principal_sha256)?;
        validate_sha256_v2("migration journal head", &self.journal_head_sha256)?;
        self.actor_generation.semantic_digest()?;
        self.state_session.semantic_digest()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationPlanV2 {
    pub transaction_id: String,
    pub source: MigrationEndpointV2,
    pub destination: MigrationEndpointV2,
    pub checkpoint_sha256: String,
}

impl MigrationPlanV2 {
    pub fn validate(&self) -> Result<()> {
        validate_identifier_v2("migration transaction", &self.transaction_id)?;
        self.source.validate()?;
        self.destination.validate()?;
        validate_sha256_v2("migration checkpoint", &self.checkpoint_sha256)?;
        if self.source.state_session.node_id() == self.destination.state_session.node_id()
            || self.source.node_public_key == self.destination.node_public_key
        {
            bail!("migration requires distinct node identities and signing keys");
        }
        if self.source.principal_sha256 != self.destination.principal_sha256 {
            bail!("migration must preserve the authenticated client principal");
        }
        let source = &self.source.actor_generation;
        let destination = &self.destination.actor_generation;
        if source.logical_environment() != destination.logical_environment()
            || source.backend_implementation() != destination.backend_implementation()
            || source.sandbox_policy() != destination.sandbox_policy()
            || source.launch_context() != destination.launch_context()
        {
            bail!("migration requires exact logical environment, backend, sandbox and launch identities");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationActionV2 {
    Prepare,
    Install,
    Fence,
    Activate,
    Abort,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationPhaseV2 {
    Prepared,
    Installing,
    Installed,
    Fenced,
    Activating,
    Activated,
    InstallFailed,
    ActivationFailed,
    Aborted,
    Cancelled,
}

impl MigrationPhaseV2 {
    pub fn blocks_execution(self) -> bool {
        matches!(
            self,
            Self::Prepared
                | Self::Installing
                | Self::Installed
                | Self::Fenced
                | Self::Activating
                | Self::ActivationFailed
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationStateV2 {
    pub plan: MigrationPlanV2,
    pub phase: MigrationPhaseV2,
    pub checkpoint_bytes: u64,
    pub actor_generation: ActorGenerationIdV1,
    pub peer_receipt_sha256: Option<String>,
    pub actor_id: Option<String>,
    pub failure: Option<HostedProtocolErrorV2>,
    pub previous_checkpoint: Option<MigrationCheckpointV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationCheckpointV2 {
    pub actor_generation: ActorGenerationIdV1,
    pub snapshot_sha256: String,
    pub snapshot_bytes: u64,
}

/// Every phase has its own exact state-control lease. This digest covers the
/// peer proof and full snapshot indirectly through their canonical hashes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationWarrantV2 {
    pub plan: MigrationPlanV2,
    pub action: MigrationActionV2,
    pub expected_journal_head_sha256: String,
    pub peer_receipt_sha256: Option<String>,
}
impl MigrationWarrantV2 {
    pub fn sha256(&self) -> Result<String> {
        self.plan.validate()?;
        validate_sha256_v2(
            "migration expected journal head",
            &self.expected_journal_head_sha256,
        )?;
        if let Some(peer) = &self.peer_receipt_sha256 {
            validate_sha256_v2("migration peer receipt", peer)?;
        }
        canonical_hosted_sha256(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrateSessionRequestV2 {
    pub credentials: SessionCredentialsV2,
    pub client_request_id: String,
    pub client_sequence: u64,
    pub warrant: MigrationWarrantV2,
    pub peer_receipt: Option<Box<SignedJournalEntryV2>>,
    pub snapshot: Option<EvaluatorStateSnapshotV1>,
    pub placement_lease: SignedPlacementLeaseV2,
}

impl MigrateSessionRequestV2 {
    pub fn validate(&self) -> Result<()> {
        self.credentials.validate()?;
        validate_client_mutation_v2(self.client_sequence, &self.client_request_id)?;
        self.warrant.sha256()?;
        let needs_peer = matches!(
            self.warrant.action,
            MigrationActionV2::Install
                | MigrationActionV2::Fence
                | MigrationActionV2::Activate
                | MigrationActionV2::Cancel
        );
        if self.peer_receipt.is_some() != needs_peer
            || self.warrant.peer_receipt_sha256.is_some() != needs_peer
            || self.snapshot.is_some() != (self.warrant.action == MigrationActionV2::Install)
        {
            bail!("migration action has missing or extraneous checkpoint/peer proof");
        }
        if let Some(peer) = &self.peer_receipt {
            peer.verify()?;
            if self.warrant.peer_receipt_sha256.as_ref() != Some(&peer.entry_sha256) {
                bail!("migration warrant does not bind the peer receipt");
            }
        }
        if let Some(snapshot) = &self.snapshot {
            snapshot.validate()?;
            if snapshot.snapshot_sha256()? != self.warrant.plan.checkpoint_sha256 {
                bail!("migration checkpoint digest differs from signed warrant");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationTransitionV2 {
    pub state: MigrationStateV2,
    pub client_sequence: u64,
    pub client_request_id: String,
    pub request_sha256: String,
    pub warrant_sha256: String,
    pub placement_lease_sha256: String,
    pub placement_lease_nonce: String,
    /// Installing/Activating allocate before any physical restore. Their
    /// terminal phase alone settles the client mutation sequence.
    pub terminal: bool,
}

pub fn migration_state_from_receipt(receipt: &SignedJournalEntryV2) -> Result<&MigrationStateV2> {
    receipt.verify()?;
    match &receipt.entry.event {
        JournalEventV2::MigrationTransition { transition } => Ok(&transition.state),
        _ => bail!("peer receipt is not a migration transition"),
    }
}
