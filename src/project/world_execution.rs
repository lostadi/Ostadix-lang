//! End-to-end hosted-reference World project execution evidence.
//!
//! This adapter enters [`super::executor::ProjectCoordinator`] through its
//! World-bound constructor, observes the terminal coordinator trace as a
//! [`super::runtime_graph::RuntimeGraphV1`], and emits one signed canonical
//! OWRECEIPT whose commit fence is always explicitly `Uncommitted`.
//! Signature and native semantic equality are evidence properties only: they
//! do not turn the launch into Governor admission, authorize residual
//! `HostWorld` effects, or claim independent native project execution.

use std::path::Path;

use anyhow::{Context, Result};

use crate::project::runtime::RunOptions;
use crate::value::OText;
use crate::world::{
    encode_signed_receipt_v1, inspect_signed_receipt_v1, project_receipt_semantic_sha256_v1,
    ComponentKindV1, ComponentObservationV1, Ed25519ReceiptSigner, ExecutionReceiptV1,
    PortableOValue, PortableValueRecord, ReceiptCommitFenceV1, ReceiptContextV1,
    ReceiptCurrentStateV1, ReceiptPlacementV1, ReceiptSubjectV1, ReceiptTerminalV1,
    SignedExecutionReceiptV1,
};

use super::deployment::{DeploymentPlanV1, PlacementSnapshotV1};
use super::executor::{ProjectCoordinator, ProjectExecutionError};
use super::launch::{HostedWorldCurrentV1, HostedWorldLaunchV1};
use super::model::{OExecutionResult, ProjectBundle};
use super::plan::ProjectHGraph;
use super::runtime_graph::{RuntimeGraphTerminalV1, RuntimeGraphV1};
use super::trace::ProjectAttemptTrace;

/// Terminal coordinator observation plus its signed uncommitted receipt.
///
/// A coordinator failure after execution began is returned as a successful
/// evidence construction with `result == None` and `coordinator_failure` set.
/// Pre-launch validation and evidence-construction failures remain ordinary
/// errors because no complete receipt can honestly be emitted for them.
#[derive(Debug)]
pub struct WorldProjectExecutionOutcome {
    pub result: Option<OExecutionResult>,
    pub coordinator_failure: Option<String>,
    pub trace: ProjectAttemptTrace,
    pub runtime_graph: RuntimeGraphV1,
    pub signed_receipt: SignedExecutionReceiptV1,
    pub receipt_semantic_sha256: [u8; 32],
}

impl WorldProjectExecutionOutcome {
    pub fn coordinator_succeeded(&self) -> bool {
        self.result.is_some() && self.coordinator_failure.is_none()
    }

    pub fn route_succeeded(&self) -> bool {
        self.result
            .as_ref()
            .is_some_and(OExecutionResult::succeeded)
    }

    pub fn receipt_hex(&self) -> String {
        hex::encode(self.signed_receipt.bytes())
    }
}

/// Execute one exact current World-hosted project launch and emit evidence.
///
/// The caller supplies the receipt signer; this API never embeds a test key or
/// invents signer trust. The receipt is signed for integrity but its commit
/// field is unconditionally [`ReceiptCommitFenceV1::Uncommitted`].
#[allow(clippy::too_many_arguments)]
pub fn execute_world_project_with_receipt(
    bundle: &ProjectBundle,
    project: &ProjectHGraph,
    opts: &RunOptions,
    deployment: &DeploymentPlanV1,
    snapshot: &PlacementSnapshotV1,
    launch: &HostedWorldLaunchV1,
    current: &HostedWorldCurrentV1,
    signer: &Ed25519ReceiptSigner,
) -> Result<WorldProjectExecutionOutcome> {
    // Keep construction separate from execution: every launch/source/current
    // fence is evaluated before a workspace or child process can exist.
    let coordinator = ProjectCoordinator::new_world_bound(
        bundle, project, opts, deployment, snapshot, launch, current,
    )?;
    match coordinator.execute() {
        Ok(execution) => {
            let runtime_graph = RuntimeGraphV1::from_project_result(
                project,
                deployment,
                launch,
                &execution.trace,
                &execution.result,
            )
            .context("failed to construct terminal project RuntimeGraph")?;
            let receipt_terminal = if execution.result.succeeded() {
                ReceiptTerminalV1::Success(success_terminal_value(
                    &runtime_graph,
                    &execution.result,
                )?)
            } else {
                let failure_code = if execution.result.was_guard_skipped() {
                    "project-route-guard-skipped"
                } else {
                    "project-route-settled-failure"
                };
                ReceiptTerminalV1::failure(
                    failure_code,
                    runtime_graph
                        .digest()
                        .context("failed to digest non-success project RuntimeGraph")?,
                )?
            };
            let (signed_receipt, receipt_semantic_sha256) =
                emit_receipt(launch, current, &runtime_graph, receipt_terminal, signer)?;
            Ok(WorldProjectExecutionOutcome {
                result: Some(execution.result),
                coordinator_failure: None,
                trace: execution.trace,
                runtime_graph,
                signed_receipt,
                receipt_semantic_sha256,
            })
        }
        Err(error) => {
            let Some(failure) = error.downcast_ref::<ProjectExecutionError>() else {
                return Err(error).context(
                    "World-hosted coordinator failed before a terminal trace could be observed",
                );
            };
            let failure_message = failure.message().to_owned();
            let trace = failure.trace.clone();
            let runtime_graph = RuntimeGraphV1::from_coordinator_failure(
                project,
                deployment,
                launch,
                &trace,
                failure_message.as_bytes(),
            )
            .context("failed to construct failed project RuntimeGraph")?;
            let detail = runtime_graph
                .digest()
                .context("failed to digest failed project RuntimeGraph")?;
            let (signed_receipt, receipt_semantic_sha256) = emit_receipt(
                launch,
                current,
                &runtime_graph,
                ReceiptTerminalV1::failure("project-coordinator-failure", detail)?,
                signer,
            )?;
            Ok(WorldProjectExecutionOutcome {
                result: None,
                coordinator_failure: Some(failure_message),
                trace,
                runtime_graph,
                signed_receipt,
                receipt_semantic_sha256,
            })
        }
    }
}

/// Write the canonical signed receipt as one lowercase even-length hex line.
/// This is the input format consumed by the native Mode 32 comparison smoke.
pub fn write_world_project_receipt_hex(
    path: &Path,
    outcome: &WorldProjectExecutionOutcome,
) -> Result<()> {
    let mut encoded = outcome.receipt_hex().into_bytes();
    encoded.push(b'\n');
    std::fs::write(path, encoded).with_context(|| {
        format!(
            "failed to write World project receipt hex to {}",
            path.display()
        )
    })
}

fn success_terminal_value(
    runtime_graph: &RuntimeGraphV1,
    result: &OExecutionResult,
) -> Result<PortableValueRecord> {
    let runtime_graph_digest = runtime_graph
        .digest()
        .context("failed to digest route-settlement project RuntimeGraph")?;
    let residual_host_world = match &runtime_graph.terminal {
        RuntimeGraphTerminalV1::RouteSettlement {
            residual_host_world,
            ..
        } => *residual_host_world,
        RuntimeGraphTerminalV1::CoordinatorFailure { .. } => {
            anyhow::bail!("success terminal requested for a failed RuntimeGraph")
        }
    };
    let exit_code = result.exit_code.map_or(PortableOValue::Null, |value| {
        PortableOValue::integer(value).expect("i32 always fits the portable integer bound")
    });
    Ok(PortableValueRecord::Core(PortableOValue::record(vec![
        ("commit_uncommitted".to_owned(), PortableOValue::Bool(true)),
        ("exit_code".to_owned(), exit_code),
        (
            "residual_host_world".to_owned(),
            PortableOValue::Bool(residual_host_world),
        ),
        (
            "route_id".to_owned(),
            portable_text(result.route_id.clone())?,
        ),
        (
            "runtime_graph_sha256".to_owned(),
            portable_text(runtime_graph_digest.as_sha256().to_owned())?,
        ),
        (
            "settled_success".to_owned(),
            PortableOValue::Bool(result.succeeded()),
        ),
    ])?))
}

fn portable_text(value: String) -> Result<PortableOValue> {
    Ok(PortableOValue::text(OText {
        utf8: value,
        encoding: Some("utf-8".to_owned()),
    })?)
}

fn emit_receipt(
    launch: &HostedWorldLaunchV1,
    current: &HostedWorldCurrentV1,
    runtime_graph: &RuntimeGraphV1,
    terminal: ReceiptTerminalV1,
    signer: &Ed25519ReceiptSigner,
) -> Result<(SignedExecutionReceiptV1, [u8; 32])> {
    let observer = launch.coordinator_observer();
    let placement = ReceiptPlacementV1::new(
        observer.node.clone(),
        observer.domain.clone(),
        observer.process.clone(),
        Vec::new(),
    )?;
    let context = ReceiptContextV1::new(
        launch.receipt().clone(),
        launch.world().clone(),
        launch.governor().clone(),
        launch.coordinator_attempt().clone(),
        placement,
    )?;
    let subject = ReceiptSubjectV1::new(
        None,
        Some(launch.project_bundle().clone()),
        None,
        Some(launch.logical_hgraph().clone()),
        None,
    )?;
    let components = vec![ComponentObservationV1::new(
        ComponentKindV1::Project,
        "project/world-hosted-reference",
        0,
        launch.project_bundle().clone(),
    )?];
    let receipt = ExecutionReceiptV1::new(
        context,
        subject,
        components,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        terminal,
        ReceiptCommitFenceV1::Uncommitted,
        None,
    )?;

    let current_observer = current.coordinator_observer();
    let receipt_current = ReceiptCurrentStateV1::new(
        current.world().clone(),
        current.governor().clone(),
        current_observer.node.clone(),
        current_observer.domain.clone(),
        current_observer.process.clone(),
        current.coordinator_attempt().clone(),
        Vec::new(),
    )?;
    let bytes = encode_signed_receipt_v1(&receipt, &receipt_current, signer)
        .context("failed to encode signed uncommitted project receipt")?;
    let receipt_semantic_sha256 = project_receipt_semantic_sha256_v1(&bytes)
        .context("failed to compute hosted project receipt semantic digest")?;
    let signed_receipt = inspect_signed_receipt_v1(&bytes)
        .context("emitted project receipt failed canonical inspection")?;

    // Keep this dependency explicit even though the runtime graph digest is
    // carried in the receipt terminal rather than a governed-effects field.
    runtime_graph
        .validate()
        .context("receipt references an invalid RuntimeGraph")?;
    Ok((signed_receipt, receipt_semantic_sha256))
}
