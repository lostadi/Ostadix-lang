//! Fail-closed admission for executing a route selected by a prior validated
//! benchmark run.
//!
//! The caller must supply a terminal record loaded through the verified run
//! store reader.  Admission rebuilds the current benchmark contract, compares
//! every content and planning coordinate, then derives an explicit one-route
//! intent from the same in-memory bundle.  No receipt file is accepted here.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::{
    prepare_execution_intent, PrepareExecutionOptionsV1, PreparedExecutionIntentV1,
    PreparedProjectExecutionV1, ProjectExecutorV1, ProjectPlanIdentitiesV1,
    ProjectSelectionReuseBindingV1, ProjectSelectionReuseObservationV1, RunDispositionV1,
    RunInputKindV1, VerifiedTerminalRunV1,
};
use crate::project::{
    build_project_hgraph, check_selection_reuse_output, DeploymentPlanV1, OExecutionResult,
    RoutePolicy, SelectionReuseContractV1, SelectionReuseOutputCheckV1,
    SelectionReuseOutputStatusV1,
};

/// Opaque process-local admission token attached to a prepared project.
///
/// Construction is possible only through [`prepare_selection_reuse_intent`].
/// It is intentionally neither serializable nor cloneable.
#[derive(Debug)]
pub struct PreparedSelectionReuseV1 {
    binding: ProjectSelectionReuseBindingV1,
    admitted_input_kind: super::IntentInputKindV1,
    admitted_input_path: PathBuf,
}

impl PreparedSelectionReuseV1 {
    pub fn binding(&self) -> &ProjectSelectionReuseBindingV1 {
        &self.binding
    }
}

/// Execute-time failure that retains the selected result and postcondition
/// without authorizing a fallback or replay.
#[derive(Debug)]
pub struct SelectionReuseExecutionErrorV1 {
    message: String,
    public_message: String,
    pub results: Vec<OExecutionResult>,
    pub observation: ProjectSelectionReuseObservationV1,
}

impl SelectionReuseExecutionErrorV1 {
    pub fn public_message(&self) -> &str {
        &self.public_message
    }

    pub(crate) fn from_check(
        results: Vec<OExecutionResult>,
        observation: ProjectSelectionReuseObservationV1,
    ) -> Self {
        let public_message = match observation.output_check.status {
            SelectionReuseOutputStatusV1::RouteFailed => {
                "the reused selected route did not complete successfully"
            }
            SelectionReuseOutputStatusV1::DeclaredOutputMismatch => {
                "the reused selected route no longer matches its validated declared output"
            }
            SelectionReuseOutputStatusV1::ObservationInvalid => {
                "the reused selected route did not produce valid output evidence"
            }
            SelectionReuseOutputStatusV1::Matched => {
                "the reused selected route failed after its output postcondition matched"
            }
        }
        .to_string();
        Self {
            message: public_message.clone(),
            public_message,
            results,
            observation,
        }
    }

    pub(crate) fn before_result(
        prepared: &PreparedProjectExecutionV1,
        _error: &anyhow::Error,
    ) -> Result<Self> {
        let reuse = prepared
            .selection_reuse()
            .context("selection-reuse failure has no admitted binding")?;
        let output_check = SelectionReuseOutputCheckV1 {
            schema: crate::project::SELECTION_REUSE_OUTPUT_CHECK_SCHEMA_V1.to_string(),
            status: SelectionReuseOutputStatusV1::ObservationInvalid,
            expected_declared_output_sha256: reuse
                .binding()
                .contract
                .expected_declared_output_sha256
                .clone(),
            observed_declared_output_sha256: None,
        };
        let observation =
            ProjectSelectionReuseObservationV1::from_binding(reuse.binding(), output_check)
                .map_err(anyhow::Error::msg)?;
        Ok(Self {
            message: "selected-route reuse failed before a terminal result was available"
                .to_string(),
            public_message:
                "the reused selected route did not produce valid terminal output evidence"
                    .to_string(),
            results: Vec::new(),
            observation,
        })
    }
}

impl fmt::Display for SelectionReuseExecutionErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SelectionReuseExecutionErrorV1 {}

/// Prepare one explicit selected-route execution from an exact verified local
/// optimization run.
///
/// The input is assembled exactly once. Admission first plans it under the
/// original benchmark contract, then consumes that prepared value and derives
/// the explicit winner plan from the same bundle in memory.
pub fn prepare_selection_reuse_intent(
    input: &Path,
    mut options: PrepareExecutionOptionsV1,
    source: &VerifiedTerminalRunV1,
) -> Result<PreparedExecutionIntentV1> {
    validate_reuse_options(&options)?;
    source
        .validate()
        .context("selection source run failed verified-store validation")?;
    let source_record = source.record();
    if source_record.disposition != RunDispositionV1::Succeeded
        || source_record.failure.is_some()
        || source_record.intent.engine != ProjectExecutorV1::Compatibility.token()
        || matches!(source_record.input.kind, RunInputKindV1::OrdinaryO)
        || source_record.intent.selection_reuse.is_some()
    {
        bail!(
            "selection source must be a successful local compatibility benchmark run, not a reused or remote execution"
        );
    }
    let receipt = source_record
        .validated_selection_receipt
        .as_ref()
        .context("selection source run has no validated-selection receipt")?;

    options.route = Some(receipt.target.clone());
    options.route_policy = Some(RoutePolicy::BenchmarkValidateAndSelect);
    let prepared = prepare_execution_intent(input, options)?;
    let PreparedExecutionIntentV1::Project(mut project) = prepared else {
        bail!("selection reuse requires a project directory or lifted project bundle");
    };
    if project.executor != ProjectExecutorV1::Compatibility
        || project.mesh.is_some()
        || project.effective_policy != RoutePolicy::BenchmarkValidateAndSelect.token()
    {
        bail!("selection reuse v1 requires the local compatibility project executor");
    }
    if project.identities.bundle_sha256 != source_record.input.digest_sha256
        || source_record.plan.hgraph_sha256.as_deref()
            != Some(project.identities.logical_hgraph_sha256.as_str())
        || source_record.plan.deployment_sha256.as_deref()
            != Some(project.identities.deployment_plan_sha256.as_str())
        || source_record.intent.target.as_deref() != Some(receipt.target.as_str())
        || source_record.intent.route_declarations != project.route_declaration_sha256
    {
        bail!(
            "the current project or benchmark plan does not exactly match the selection source run"
        );
    }

    let contract = SelectionReuseContractV1::from_current_project(
        &project.bundle,
        receipt,
        &project.identities.logical_hgraph_sha256,
        &project.identities.deployment_plan_sha256,
        project.route_declaration_sha256.clone(),
    )
    .map_err(anyhow::Error::msg)
    .context("selection reuse was not admitted")?;
    let binding = ProjectSelectionReuseBindingV1::new(
        source_record.run_id.clone(),
        source_record.sequence,
        source.record_ref().clone(),
        receipt.clone(),
        contract,
    )
    .map_err(anyhow::Error::msg)
    .context("failed to bind selection-reuse evidence")?;

    derive_explicit_reuse_project(&mut project, binding)?;
    Ok(PreparedExecutionIntentV1::Project(project))
}

fn validate_reuse_options(options: &PrepareExecutionOptionsV1) -> Result<()> {
    if options.route.is_some() || options.route_policy.is_some() {
        bail!("selection reuse supplies its route set and policy from the source run");
    }
    if options.parallel_auto
        || options.explicit_mesh
        || options.mesh.is_some()
        || options.ordinary_executor.is_some()
        || options.local_workers.is_some()
        || !options.backend_grants.is_empty()
        || !options.excluded_project_paths.is_empty()
    {
        bail!(
            "selection reuse v1 cannot be combined with mesh, parallel, ordinary-evaluator, or project-exclusion controls"
        );
    }
    Ok(())
}

fn derive_explicit_reuse_project(
    project: &mut PreparedProjectExecutionV1,
    binding: ProjectSelectionReuseBindingV1,
) -> Result<()> {
    let target = binding.contract.target.clone();
    let selected = binding.contract.selected_route_id.clone();
    let policy = RoutePolicy::Explicit(selected);
    let graph = build_project_hgraph(&project.bundle, Some(&target), Some(policy.clone()))
        .map_err(anyhow::Error::msg)
        .context("failed to derive the selected-route reuse graph")?;
    super::validate_project_executor_preflight(
        &project.bundle,
        &graph,
        ProjectExecutorV1::Compatibility,
    )?;
    let logical = graph
        .logical_v1()
        .context("failed to normalize selected-route reuse HGraph")?;
    let logical_digest = logical
        .digest()
        .context("failed to digest selected-route reuse HGraph")?;
    let deployment = DeploymentPlanV1::hosted(&logical)
        .context("failed to construct selected-route reuse deployment")?;
    let deployment_digest = deployment
        .digest()
        .context("failed to digest selected-route reuse deployment")?;
    if graph.plan.bundle_digest != binding.contract.bundle_sha256 {
        bail!("selected-route reuse graph changed the admitted project bundle identity");
    }

    project.identities = ProjectPlanIdentitiesV1 {
        bundle_sha256: graph.plan.bundle_digest.clone(),
        logical_hgraph_sha256: logical_digest.as_sha256().to_string(),
        deployment_plan_sha256: deployment_digest.as_sha256().to_string(),
    };
    project.route = Some(target);
    project.policy = Some(policy);
    project.selected_target = graph.plan.target.clone();
    project.effective_policy = graph.plan.policy.token();
    project.static_plan = super::render_project_hgraph_static_plan(&graph)?;
    project.selection_reuse = Some(Box::new(PreparedSelectionReuseV1 {
        binding,
        admitted_input_kind: project.input_kind,
        admitted_input_path: project.input_path.clone(),
    }));
    Ok(())
}

/// Revalidate every mutable prepared-project coordinate immediately before
/// dispatch. `PreparedProjectExecutionV1` predates the sealed reuse token and
/// remains publicly mutable, so preparation alone is not a sufficient trust
/// boundary for embedders.
pub(crate) fn validate_prepared_selection_reuse(
    prepared: &PreparedProjectExecutionV1,
) -> Result<()> {
    let reuse = prepared
        .selection_reuse()
        .context("selected-route execution has no admitted reuse binding")?;
    let binding = reuse.binding();
    binding
        .validate()
        .map_err(anyhow::Error::msg)
        .context("selected-route reuse binding is invalid")?;

    let rebuilt_contract = SelectionReuseContractV1::from_current_project(
        &prepared.bundle,
        &binding.receipt,
        &binding.contract.benchmark_hgraph_sha256,
        &binding.contract.benchmark_deployment_sha256,
        prepared.route_declaration_sha256.clone(),
    )
    .map_err(anyhow::Error::msg)
    .context("selected-route reuse contract no longer matches the prepared bundle")?;
    if rebuilt_contract != binding.contract
        || prepared.input_kind != reuse.admitted_input_kind
        || prepared.input_path != reuse.admitted_input_path
        || prepared.identities.bundle_sha256 != binding.contract.bundle_sha256
        || prepared.route.as_deref() != Some(binding.contract.target.as_str())
        || prepared.policy
            != Some(RoutePolicy::Explicit(
                binding.contract.selected_route_id.clone(),
            ))
        || prepared.selected_target != binding.contract.target
        || prepared.effective_policy
            != RoutePolicy::Explicit(binding.contract.selected_route_id.clone()).token()
        || prepared.route_declaration_sha256 != binding.contract.route_declaration_sha256
        || prepared.parallel_auto
        || prepared.executor != ProjectExecutorV1::Compatibility
        || prepared.mesh.is_some()
    {
        bail!("prepared selected-route execution differs from its admitted reuse contract");
    }

    let graph = build_project_hgraph(
        &prepared.bundle,
        Some(&binding.contract.target),
        Some(RoutePolicy::Explicit(
            binding.contract.selected_route_id.clone(),
        )),
    )
    .map_err(anyhow::Error::msg)
    .context("failed to rebuild the admitted selected-route graph")?;
    super::validate_project_executor_preflight(
        &prepared.bundle,
        &graph,
        ProjectExecutorV1::Compatibility,
    )?;
    let logical = graph
        .logical_v1()
        .context("failed to normalize the admitted selected-route HGraph")?;
    let deployment = DeploymentPlanV1::hosted(&logical)
        .context("failed to rebuild the admitted selected-route deployment")?;
    let expected_identities = ProjectPlanIdentitiesV1 {
        bundle_sha256: graph.plan.bundle_digest.clone(),
        logical_hgraph_sha256: logical
            .digest()
            .context("failed to digest the admitted selected-route HGraph")?
            .as_sha256()
            .to_string(),
        deployment_plan_sha256: deployment
            .digest()
            .context("failed to digest the admitted selected-route deployment")?
            .as_sha256()
            .to_string(),
    };
    if prepared.identities != expected_identities
        || prepared.static_plan != super::render_project_hgraph_static_plan(&graph)?
    {
        bail!("prepared selected-route plan differs from its admitted reuse contract");
    }
    Ok(())
}

pub(crate) fn observe_reused_result(
    prepared: &PreparedProjectExecutionV1,
    result: &OExecutionResult,
) -> Result<ProjectSelectionReuseObservationV1> {
    let reuse = prepared
        .selection_reuse()
        .context("selected-route output check has no admitted reuse binding")?;
    let check = check_selection_reuse_output(&prepared.bundle, &reuse.binding().contract, result);
    ProjectSelectionReuseObservationV1::from_binding(reuse.binding(), check)
        .map_err(anyhow::Error::msg)
}

pub(crate) fn observe_invalid_reuse_result(
    prepared: &PreparedProjectExecutionV1,
) -> Result<ProjectSelectionReuseObservationV1> {
    let reuse = prepared
        .selection_reuse()
        .context("selected-route output check has no admitted reuse binding")?;
    let check = SelectionReuseOutputCheckV1 {
        schema: crate::project::SELECTION_REUSE_OUTPUT_CHECK_SCHEMA_V1.to_string(),
        status: SelectionReuseOutputStatusV1::ObservationInvalid,
        expected_declared_output_sha256: reuse
            .binding()
            .contract
            .expected_declared_output_sha256
            .clone(),
        observed_declared_output_sha256: None,
    };
    ProjectSelectionReuseObservationV1::from_binding(reuse.binding(), check)
        .map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::*;
    use crate::intent::{
        execute_prepared_intent, route_result_references, CapturedStreamV1, ExecutionObservationV1,
        PreparedExecutionIntentV1, RecordedRouteResultV1, RunRecordV1, RunSelectorV1,
        RunStoreReaderV1, RunStoreV1, RunTraceBindingV1,
    };
    use crate::project::executor::PROJECT_EXECUTOR_ENV;

    static PROJECT_EXECUTOR_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Clone, Copy)]
    enum EffectBoundaryFixture {
        Pure,
        UnknownAlternative,
        UnknownPrerequisite,
    }

    struct ExecutorEnvRestore(Option<OsString>);

    impl ExecutorEnvRestore {
        fn unset() -> Self {
            let original = std::env::var_os(PROJECT_EXECUTOR_ENV);
            std::env::remove_var(PROJECT_EXECUTOR_ENV);
            Self(original)
        }
    }

    impl Drop for ExecutorEnvRestore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var(PROJECT_EXECUTOR_ENV, value),
                None => std::env::remove_var(PROJECT_EXECUTOR_ENV),
            }
        }
    }

    #[derive(Clone)]
    struct FixtureMarkers {
        reference: PathBuf,
        prerequisite: PathBuf,
        selected: PathBuf,
    }

    impl FixtureMarkers {
        fn clear(&self) {
            for path in [&self.reference, &self.prerequisite, &self.selected] {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => panic!("failed to remove {}: {error}", path.display()),
                }
            }
        }

        fn assert_absent(&self) {
            for path in [&self.reference, &self.prerequisite, &self.selected] {
                assert!(
                    !path.exists(),
                    "reuse admission unexpectedly dispatched {}",
                    path.display()
                );
            }
        }
    }

    fn toml_string(value: &Path) -> String {
        format!("{:?}", value.to_string_lossy())
    }

    fn project_fixture(root: &Path, boundary: EffectBoundaryFixture) -> (PathBuf, FixtureMarkers) {
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let markers = FixtureMarkers {
            reference: root.join("reference.marker"),
            prerequisite: root.join("prerequisite.marker"),
            selected: root.join("selected.marker"),
        };
        let shell = which::which("sh").expect("the reuse API tests require a POSIX shell");
        let alternative_purity = match boundary {
            EffectBoundaryFixture::UnknownAlternative => "",
            EffectBoundaryFixture::Pure | EffectBoundaryFixture::UnknownPrerequisite => {
                "pure = true"
            }
        };
        let prerequisite_purity = match boundary {
            EffectBoundaryFixture::UnknownPrerequisite => "",
            EffectBoundaryFixture::Pure | EffectBoundaryFixture::UnknownAlternative => {
                "pure = true"
            }
        };
        let manifest = format!(
            r#"[project]
name = "reuse-api-integrity"

[[routes]]
id = "reference"
command = [{shell}, "-c", 'printf reference >> "$MARKER"; sleep 0.08; printf stable']
env = {{ MARKER = {reference_marker} }}
pure = true

[[routes]]
id = "selected-prerequisite"
command = [{shell}, "-c", 'printf prerequisite >> "$MARKER"']
env = {{ MARKER = {prerequisite_marker} }}
{prerequisite_purity}

[[routes]]
id = "selected"
command = [{shell}, "-c", 'printf selected >> "$MARKER"; printf stable']
env = {{ MARKER = {selected_marker} }}
depends_on = ["selected-prerequisite"]
{alternative_purity}

[[route_sets]]
provides = "main"
alternatives = ["reference", "selected"]
policy = "benchmark_validate_and_select"
"#,
            shell = toml_string(&shell),
            reference_marker = toml_string(&markers.reference),
            prerequisite_marker = toml_string(&markers.prerequisite),
            selected_marker = toml_string(&markers.selected),
        );
        fs::write(project.join("olang.project.toml"), manifest).unwrap();
        (project, markers)
    }

    fn benchmark_options() -> PrepareExecutionOptionsV1 {
        PrepareExecutionOptionsV1 {
            route: Some("main".to_string()),
            route_policy: Some(RoutePolicy::BenchmarkValidateAndSelect),
            ..PrepareExecutionOptionsV1::default()
        }
    }

    fn retain_benchmark_source(project: &Path, store_root: &Path) -> VerifiedTerminalRunV1 {
        let prepared = prepare_execution_intent(project, benchmark_options()).unwrap();
        let store = RunStoreV1::open_at(store_root).unwrap();
        let lease = store.begin(prepared.run_attempt_seed(1).unwrap()).unwrap();
        let attempt = lease.attempt().clone();
        let observation = execute_prepared_intent(&prepared).unwrap();
        let ExecutionObservationV1::Project(observation) = observation else {
            panic!("project benchmark unexpectedly produced an ordinary-O observation");
        };
        let receipt = observation
            .validated_selection_receipt
            .expect("benchmark execution produced no validated-selection receipt");
        assert_eq!(receipt.selected_route_id, "selected");
        let measurements = observation
            .validated_selection_measurements
            .expect("benchmark execution produced no independent measurements");
        let mut route_results = observation
            .results
            .iter()
            .map(RecordedRouteResultV1::from)
            .collect::<Vec<_>>();
        for candidate in &receipt.candidates {
            let result = route_results
                .iter_mut()
                .find(|result| result.route_id == candidate.route_id)
                .expect("receipt candidate has no retained route result");
            let measurement = measurements
                .iter()
                .find(|measurement| measurement.route_id == candidate.route_id)
                .expect("receipt candidate has no independent measurement");
            result.result_codec = Some(measurement.result_codec);
            result.branch_elapsed_ns = Some(measurement.branch_elapsed_ns.to_string());
        }
        let elapsed_nanos = receipt
            .candidates
            .iter()
            .flat_map(|candidate| {
                [
                    candidate.terminal_elapsed_ns.parse::<u128>().unwrap(),
                    candidate.branch_elapsed_ns.parse::<u128>().unwrap(),
                ]
            })
            .max()
            .and_then(|elapsed| u64::try_from(elapsed).ok())
            .and_then(|elapsed| elapsed.checked_add(1))
            .unwrap();
        let decoded_value = observation
            .results
            .iter()
            .find(|result| result.route_id == receipt.selected_route_id)
            .and_then(|result| result.value.clone());
        let mut record = RunRecordV1::terminal(
            attempt.run_id.clone(),
            attempt.sequence,
            &attempt.seed,
            1_u64.checked_add(elapsed_nanos).unwrap(),
            elapsed_nanos,
            RunDispositionV1::Succeeded,
            CapturedStreamV1::default(),
            CapturedStreamV1::default(),
            decoded_value,
            route_results.clone(),
            route_result_references(&route_results),
            RunTraceBindingV1::unavailable(
                "compatibility benchmark has no retained lifecycle trace",
            ),
            None,
        );
        record.validated_selection_receipt = Some((*receipt).clone());
        record.validate().unwrap();
        let finalized = lease.finalize(record, None).unwrap();
        RunStoreReaderV1::open_existing(store.root())
            .unwrap()
            .read_terminal_verified(RunSelectorV1::RunId(finalized.run_id), false)
            .unwrap()
    }

    fn admitted_project(
        project: &Path,
        source: &VerifiedTerminalRunV1,
    ) -> PreparedExecutionIntentV1 {
        prepare_selection_reuse_intent(project, PrepareExecutionOptionsV1::default(), source)
            .unwrap()
    }

    fn assert_pre_dispatch_rejection(
        prepared: &PreparedExecutionIntentV1,
        markers: &FixtureMarkers,
    ) {
        let error = execute_prepared_intent(prepared).unwrap_err();
        let reuse = error
            .downcast_ref::<SelectionReuseExecutionErrorV1>()
            .expect("mutated reuse intent did not fail through the typed reuse boundary");
        assert!(reuse.results.is_empty());
        assert_eq!(
            reuse.observation.output_check.status,
            SelectionReuseOutputStatusV1::ObservationInvalid
        );
        markers.assert_absent();
    }

    #[test]
    fn selected_route_reuse_is_pinned_and_revalidates_public_prepared_fields() {
        let _env_lock = PROJECT_EXECUTOR_ENV_LOCK.lock().unwrap();
        let _executor_env = ExecutorEnvRestore::unset();
        let temporary = tempfile::tempdir().unwrap();
        let (project, markers) = project_fixture(temporary.path(), EffectBoundaryFixture::Pure);
        let source = retain_benchmark_source(&project, &temporary.path().join("runs"));

        markers.clear();
        let pinned = admitted_project(&project, &source);
        let PreparedExecutionIntentV1::Project(pinned_project) = &pinned else {
            panic!("selection reuse did not prepare a project intent");
        };
        assert_eq!(pinned_project.executor, ProjectExecutorV1::Compatibility);
        std::env::set_var(PROJECT_EXECUTOR_ENV, "hgraph");
        let executed = execute_prepared_intent(&pinned).unwrap();
        std::env::remove_var(PROJECT_EXECUTOR_ENV);
        let ExecutionObservationV1::Project(executed) = executed else {
            panic!("selected route unexpectedly executed as ordinary O");
        };
        assert_eq!(executed.results.len(), 1);
        assert_eq!(executed.results[0].route_id, "selected");
        assert!(executed.selection_reuse.unwrap().output_check.matched());
        assert!(!markers.reference.exists());
        assert_eq!(fs::read(&markers.prerequisite).unwrap(), b"prerequisite");
        assert_eq!(fs::read(&markers.selected).unwrap(), b"selected");

        markers.clear();
        let mut changed_policy = admitted_project(&project, &source);
        {
            let PreparedExecutionIntentV1::Project(project) = &mut changed_policy else {
                unreachable!();
            };
            project.policy = Some(RoutePolicy::Explicit("reference".to_string()));
        }
        assert_pre_dispatch_rejection(&changed_policy, &markers);

        markers.clear();
        let mut changed_bundle = admitted_project(&project, &source);
        let PreparedExecutionIntentV1::Project(changed_bundle_project) = &mut changed_bundle else {
            unreachable!();
        };
        changed_bundle_project
            .bundle
            .metadata
            .insert("post-admission".to_string(), "tampered".to_string());
        assert_pre_dispatch_rejection(&changed_bundle, &markers);

        markers.clear();
        let mut changed_mesh = admitted_project(&project, &source);
        let PreparedExecutionIntentV1::Project(changed_mesh_project) = &mut changed_mesh else {
            unreachable!();
        };
        changed_mesh_project.mesh =
            Some(crate::hosted_remote::project_mesh::MeshExecutionConfig::default());
        assert_pre_dispatch_rejection(&changed_mesh, &markers);

        markers.clear();
        let mut changed_input_kind = admitted_project(&project, &source);
        let PreparedExecutionIntentV1::Project(changed_input_kind_project) =
            &mut changed_input_kind
        else {
            unreachable!();
        };
        changed_input_kind_project.input_kind = crate::intent::IntentInputKindV1::LiftedProject;
        assert_pre_dispatch_rejection(&changed_input_kind, &markers);

        markers.clear();
        let mut changed_input_path = admitted_project(&project, &source);
        let PreparedExecutionIntentV1::Project(changed_input_path_project) =
            &mut changed_input_path
        else {
            unreachable!();
        };
        changed_input_path_project.input_path = project.join("forged-record-attribution");
        assert_pre_dispatch_rejection(&changed_input_path, &markers);
    }

    #[test]
    fn unknown_alternative_or_prerequisite_is_never_admitted_for_reuse() {
        let _env_lock = PROJECT_EXECUTOR_ENV_LOCK.lock().unwrap();
        let _executor_env = ExecutorEnvRestore::unset();
        for (name, boundary) in [
            (
                "unknown-alternative",
                EffectBoundaryFixture::UnknownAlternative,
            ),
            (
                "unknown-prerequisite",
                EffectBoundaryFixture::UnknownPrerequisite,
            ),
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join(name);
            fs::create_dir_all(&root).unwrap();
            let (project, markers) = project_fixture(&root, boundary);
            let source = retain_benchmark_source(&project, &root.join("runs"));
            markers.clear();

            let error = prepare_selection_reuse_intent(
                &project,
                PrepareExecutionOptionsV1::default(),
                &source,
            )
            .unwrap_err();
            assert!(
                format!("{error:#}").contains("not explicitly declared pure"),
                "unexpected {name} admission error: {error:#}"
            );
            markers.assert_absent();
        }
    }
}
