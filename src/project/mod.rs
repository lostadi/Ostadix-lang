//! First-class project, route, and bundle model for O-lang.
//!
//! This module gives O-lang a lossless, route-preserving representation of an
//! entire codebase:
//!
//!   * [`model`] — the core serde vocabulary (bundles, files, routes, sets,
//!     policies, guards, results).
//!   * [`bundle`] — lossless directory bundling and (de)serialization.
//!   * [`materialize`] — safe workspace materialization on disk.
//!   * [`manifest`] — `olang.project.toml` parsing and CLI route overrides.
//!   * [`discover`] + [`ecosystems`] — automatic ecosystem route discovery.
//!   * [`runtime`] — native route execution with prerequisites and policies.
//!   * [`lower`] — lifting a project into a single valid `.O` document.

use anyhow::Result;
use std::path::{Path, PathBuf};

pub mod bundle;
pub mod deployment;
pub mod discover;
pub mod ecosystems;
pub mod executor;
pub mod logical;
pub mod lower;
pub mod manifest;
pub mod materialize;
pub mod model;
pub mod plan;
pub mod runtime;
pub mod trace;

pub use deployment::{
    DeploymentArchitectureRequirementV1, DeploymentCompatibilityIssueV1,
    DeploymentFailureDomainConstraintV1, DeploymentOperationBindingV1,
    DeploymentOperationRequirementsV1, DeploymentOperationV1, DeploymentPlanError,
    DeploymentPlanV1, DeploymentProviderBindingV1, DeploymentProviderIssueV1,
    DeploymentProviderRejectionV1, DeploymentProviderSnapshotV1, PlacementSnapshotV1,
    DEPLOYMENT_PLAN_SCHEMA_V1, MAX_DEPLOYMENT_OPERATIONS, MAX_DEPLOYMENT_PROVIDERS,
    MAX_DEPLOYMENT_RECORD_BYTES, MAX_DEPLOYMENT_REQUIREMENTS, PLACEMENT_SNAPSHOT_SCHEMA_V1,
};
pub use executor::{
    execute_project_hgraph, execute_project_hgraph_selection,
    execute_selection_with_configured_executor, run_selection_with_configured_executor,
    write_project_attempt_trace, ConfiguredProjectExecution, ProjectCoordinator,
    ProjectExecutionError, ProjectExecutionOutcome,
};
pub use logical::{
    LogicalArtifactRefV1, LogicalArtifactRoleV1, LogicalAuthorityRequirementV1,
    LogicalCancellationV1, LogicalDependencyKindV1, LogicalDependencyV1, LogicalEffectConfidenceV1,
    LogicalEffectSummaryV1, LogicalFailureContinuationV1, LogicalFallibilityV1, LogicalHGraphError,
    LogicalHGraphV1, LogicalOperationIdV1, LogicalOperationKindV1, LogicalOperationV1,
    LogicalProjectSourceV1, LogicalResourceV1, LogicalRouteFactsV1, LogicalRouteGuardV1,
    LogicalRouteKindV1, LogicalRoutePolicyV1, LOGICAL_HGRAPH_SCHEMA_V1, MAX_LOGICAL_HGRAPH_BYTES,
};
pub use model::{
    Artifact, ExecutionProvenance, FileRole, OExecutionResult, ProjectBundle, ProjectFile,
    ResultCodec, RouteEffects, RouteExecutionDisposition, RouteFailureContinuation, RouteGuard,
    RouteKind, RoutePolicy, RouteProvenance, RouteSet, RouteSpec,
};
pub use plan::{
    build_project_hgraph, ProjectCancellationSemantics, ProjectDependency, ProjectExecutionPlan,
    ProjectHGraph, ProjectPlanOperation, RoutePlanFacts,
};
pub use trace::{
    ProjectArtifactFingerprint, ProjectAttemptEvent, ProjectAttemptIdentity, ProjectAttemptState,
    ProjectAttemptTrace, ProjectAttemptTraceHeader, ProjectContinuationDecision,
    ProjectContinuationEvidence, ProjectRouteOutcome, ProjectTraceError,
};

/// Derive a project name from a directory path.
pub fn name_from_path(root: &Path) -> String {
    root.canonicalize()
        .ok()
        .as_deref()
        .and_then(|p| p.file_name())
        .or_else(|| root.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .filter(|s| !s.is_empty() && s != ".")
        .unwrap_or_else(|| "project".to_string())
}

/// Assemble a complete [`ProjectBundle`] from a directory: bundle the files,
/// discover routes, apply the manifest, then apply CLI route overrides.
///
/// The precedence is: CLI overrides > manifest > discovery.
pub fn assemble(root: &Path, name: &str, route_decls: &[String]) -> Result<ProjectBundle> {
    assemble_excluding(root, name, route_decls, &[])
}

/// Assemble a complete [`ProjectBundle`] while excluding exact filesystem
/// paths from the captured file set.
///
/// This is primarily used by `o-link` when its output path is inside the
/// project root: an existing non-generated output must not be captured and
/// then overwritten as part of the new bundle. Relative exclusions are
/// resolved from the caller's current working directory.
pub fn assemble_excluding(
    root: &Path,
    name: &str,
    route_decls: &[String],
    exclusions: &[PathBuf],
) -> Result<ProjectBundle> {
    let mut bundle = bundle::bundle_dir_excluding(root, name, exclusions)?;
    discover::apply_discovery(&mut bundle, root);
    manifest::load_and_apply(&mut bundle, root)?;
    manifest::apply_cli_overrides(&mut bundle, route_decls)?;
    finalize_default(&mut bundle);
    Ok(bundle)
}

/// If no default route is set yet but exactly one credible run route exists,
/// adopt it as the default.
pub fn finalize_default(bundle: &mut ProjectBundle) {
    if bundle.default_route.is_some() {
        // Keep the manifest/CLI choice, but reflect it on the route flags.
        if let Some(id) = bundle.default_route.clone() {
            for route in &mut bundle.routes {
                route.is_default = route.id == id;
            }
        }
        return;
    }
    let run_candidates: Vec<String> = bundle
        .routes
        .iter()
        .filter(|r| discover::is_run_candidate(r))
        .map(|r| r.id.clone())
        .collect();
    if run_candidates.len() == 1 {
        let id = run_candidates.into_iter().next().unwrap();
        for route in &mut bundle.routes {
            route.is_default = route.id == id;
        }
        bundle.default_route = Some(id);
    }
}
