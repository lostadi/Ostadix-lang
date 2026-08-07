//! End-to-end coverage for the bounded World-hosted project evidence path.
//!
//! These tests prove exact pre-execution freshness fencing, terminal
//! RuntimeGraph observation, signed uncommitted OWRECEIPT emission, and the
//! hosted semantic digest used by the native comparison smoke. They do not
//! claim Governor admission, governed effects, or independent native project
//! execution.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use o_lang::project::runtime::{GuardBehavior, RouteExecutionError, RunOptions};
use o_lang::project::{
    self, build_project_hgraph, execute_world_project_with_receipt,
    write_world_project_receipt_hex, ArtifactCaptureFailure, ArtifactCaptureStatus,
    DeploymentArchitectureRequirementV1, DeploymentPlanV1, DeploymentProjectPathV1,
    DeploymentProviderBindingV1, DeploymentProviderSnapshotV1, HostedWorldCoordinatorObserverV1,
    HostedWorldCurrentV1, HostedWorldLaunchV1, LogicalOperationIdV1, PlacementSnapshotV1,
    ProjectAttemptState, ProjectAttemptTrace, ProjectBundle, ProjectContinuationDecision,
    ProjectContinuationEvidence, ProjectHGraph, RuntimeGraphTerminalV1, RuntimeGraphV1,
};
use o_lang::world::{
    project_receipt_semantic_sha256_v1, ArtifactId, AttemptGeneration, AttemptIdentity,
    DomainGeneration, DomainId, DomainIdentity, Ed25519ReceiptSigner, GovernorIdentity,
    GovernorLogIndex, GovernorTerm, NodeGeneration, NodeId, NodeIdentity, ProcessGeneration,
    ProcessId, ProcessIdentity, ReceiptCommitFenceV1, ReceiptId, ReceiptIdentity,
    ReceiptKeyResolver, ReceiptTerminalV1, ResourceGeneration, ResourceId, ResourceIdentity,
    ResourceOwner, TaskId, TaskIdentity, WorldEpoch, WorldId, WorldIdentity,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const TEST_RECEIPT_SECRET: [u8; 32] = [0x5a; 32];
const MARKER_ENV: &str = "PROJECT_EXEC_A_EXECUTION_MARKER";

struct ExactResolver {
    key_id: [u8; 32],
    public: [u8; 32],
}

impl ReceiptKeyResolver for ExactResolver {
    fn resolve_ed25519(&self, key_id: &[u8; 32]) -> Option<[u8; 32]> {
        (key_id == &self.key_id).then_some(self.public)
    }
}

#[derive(Clone, Copy)]
enum RouteMode {
    Success,
    NonZero,
    GuardSkipped,
    MissingArtifact,
    SpawnAbort,
}

struct WorldFixture {
    bundle: ProjectBundle,
    project: ProjectHGraph,
    snapshot: PlacementSnapshotV1,
    deployment: DeploymentPlanV1,
    launch: HostedWorldLaunchV1,
    current: HostedWorldCurrentV1,
    signer: Ed25519ReceiptSigner,
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_hgraph_exec")
}

fn artifact(label: &str) -> ArtifactId {
    ArtifactId::from_sha256(hex::encode(Sha256::digest(label.as_bytes()))).unwrap()
}

fn world(epoch: u64) -> WorldIdentity {
    WorldIdentity::new(
        WorldId::new("desk").unwrap(),
        WorldEpoch::new(epoch).unwrap(),
    )
}

fn provider_binding(
    node_generation: u64,
    domain_generation: u64,
    process_generation: u64,
    service_generation: u64,
    implementation: ArtifactId,
) -> DeploymentProviderBindingV1 {
    let world_id = WorldId::new("desk").unwrap();
    let node = NodeIdentity::new(
        world_id,
        NodeId::new("node-world").unwrap(),
        NodeGeneration::new(node_generation).unwrap(),
    );
    let domain = DomainIdentity::new(
        node.clone(),
        DomainId::new("project-host").unwrap(),
        DomainGeneration::new(domain_generation).unwrap(),
    );
    let process = ProcessIdentity::new(
        domain.clone(),
        ProcessId::new("runner").unwrap(),
        ProcessGeneration::new(process_generation).unwrap(),
    );
    let service = ResourceIdentity::new(
        ResourceOwner::Process {
            process: process.clone(),
        },
        ResourceId::new("project/executor").unwrap(),
        ResourceGeneration::new(service_generation).unwrap(),
    );
    DeploymentProviderBindingV1 {
        node,
        domain,
        process: Some(process),
        service,
        implementation,
    }
}

fn coordinator_observer(
    node_generation: u64,
    domain_generation: u64,
    process_generation: u64,
) -> HostedWorldCoordinatorObserverV1 {
    let world_id = WorldId::new("desk").unwrap();
    let node = NodeIdentity::new(
        world_id,
        NodeId::new("hosted-coordinator-node").unwrap(),
        NodeGeneration::new(node_generation).unwrap(),
    );
    let domain = DomainIdentity::new(
        node.clone(),
        DomainId::new("hosted-coordinator-domain").unwrap(),
        DomainGeneration::new(domain_generation).unwrap(),
    );
    let process = ProcessIdentity::new(
        domain.clone(),
        ProcessId::new("hosted-coordinator").unwrap(),
        ProcessGeneration::new(process_generation).unwrap(),
    );
    HostedWorldCoordinatorObserverV1::new(node, domain, Some(process)).unwrap()
}

fn compatible_provider(logical: &project::LogicalHGraphV1) -> DeploymentProviderSnapshotV1 {
    // Use the hosted derivation only as the requirements oracle. The actual
    // deployment under test is independently derived from the World snapshot.
    let hosted = DeploymentPlanV1::hosted(logical).unwrap();
    let mut runtime_classes = BTreeSet::new();
    let mut executables = BTreeSet::new();
    let mut evaluators = BTreeSet::new();
    let mut environment_keys = BTreeSet::new();
    let mut packages = BTreeSet::new();
    let mut project_paths = BTreeSet::<DeploymentProjectPathV1>::new();
    let mut required_architecture = None;
    let mut required_platform = None;

    for operation in &hosted.operations {
        let requirements = &operation.requirements;
        runtime_classes.extend(requirements.runtime_classes.iter().cloned());
        executables.extend(requirements.executables.iter().cloned());
        evaluators.extend(requirements.evaluators.iter().cloned());
        environment_keys.extend(requirements.environment_keys.iter().cloned());
        packages.extend(requirements.packages.iter().cloned());
        project_paths.extend(requirements.locality.iter().cloned());
        if let DeploymentArchitectureRequirementV1::Exact { architecture } =
            &requirements.architecture
        {
            match &required_architecture {
                None => required_architecture = Some(architecture.clone()),
                Some(current) => assert_eq!(current, architecture),
            }
        }
        for platform in &requirements.platform_os {
            match &required_platform {
                None => required_platform = Some(platform.clone()),
                Some(current) => assert_eq!(current, platform),
            }
        }
        assert!(
            requirements.authority.is_empty(),
            "a descriptive snapshot cannot satisfy authority requirements"
        );
        assert!(requirements.failure_domains.is_empty());
    }

    DeploymentProviderSnapshotV1 {
        binding: provider_binding(2, 3, 4, 5, artifact("world-provider-implementation")),
        architecture: required_architecture.unwrap_or_else(|| std::env::consts::ARCH.to_owned()),
        platform_os: required_platform.unwrap_or_else(|| std::env::consts::OS.to_owned()),
        runtime_classes: runtime_classes.into_iter().collect(),
        executables: executables.into_iter().collect(),
        evaluators: evaluators.into_iter().collect(),
        environment_keys: environment_keys.into_iter().collect(),
        packages: packages.into_iter().collect(),
        project_bundles: vec![logical.source.bundle.clone()],
        project_paths: project_paths.into_iter().collect(),
        failure_domain: "rack-local".to_owned(),
        admits_host_world: true,
    }
}

fn identities(
    logical: &project::LogicalHGraphV1,
) -> (
    BTreeMap<LogicalOperationIdV1, TaskIdentity>,
    BTreeMap<LogicalOperationIdV1, AttemptIdentity>,
) {
    let world_id = WorldId::new("desk").unwrap();
    let mut tasks = BTreeMap::new();
    let mut attempts = BTreeMap::new();
    for operation in &logical.operations {
        let task_id = TaskId::new(format!("project-op-{:04}", operation.id.0)).unwrap();
        tasks.insert(
            operation.id,
            TaskIdentity::new(world_id.clone(), task_id.clone()),
        );
        attempts.insert(
            operation.id,
            AttemptIdentity::new(
                world_id.clone(),
                task_id,
                AttemptGeneration::new(operation.id.0 + 1).unwrap(),
            ),
        );
    }
    (tasks, attempts)
}

fn fixture(marker: &Path, mode: RouteMode) -> WorldFixture {
    let mut bundle = project::assemble(&fixture_path(), "project-world-runtime", &[]).unwrap();
    let marker = marker.to_string_lossy().into_owned();
    for route in &mut bundle.routes {
        route
            .environment
            .insert(MARKER_ENV.to_owned(), marker.clone());
    }
    if matches!(mode, RouteMode::SpawnAbort) {
        let main = bundle
            .routes
            .iter_mut()
            .find(|route| route.id == "main")
            .unwrap();
        main.command[0] = "/ostadix-test/command-that-does-not-exist".to_owned();
    } else if matches!(mode, RouteMode::NonZero) {
        let main = bundle
            .routes
            .iter_mut()
            .find(|route| route.id == "main")
            .unwrap();
        main.command[2].push_str("\nexit 9\n");
    } else if matches!(mode, RouteMode::GuardSkipped) {
        let main = bundle
            .routes
            .iter_mut()
            .find(|route| route.id == "main")
            .unwrap();
        main.guards = vec![project::RouteGuard::CommandAvailable(
            "ostadix-command-that-cannot-exist".to_owned(),
        )];
    } else if matches!(mode, RouteMode::MissingArtifact) {
        let main = bundle
            .routes
            .iter_mut()
            .find(|route| route.id == "main")
            .unwrap();
        main.outputs = vec!["required-but-not-produced.bin".to_owned()];
    }

    let project = build_project_hgraph(&bundle, Some("main"), None).unwrap();
    let logical = project.logical_v1().unwrap();
    let (tasks, attempts) = identities(&logical);
    let snapshot = PlacementSnapshotV1::new(world(7), vec![compatible_provider(&logical)]).unwrap();
    let deployment =
        DeploymentPlanV1::from_snapshot_single_provider(&logical, &snapshot, &tasks).unwrap();
    assert!(deployment.selected_provider.is_some());
    let governor = GovernorIdentity::new(
        snapshot.world.clone(),
        GovernorTerm::new(3).unwrap(),
        GovernorLogIndex::new(9).unwrap(),
    );
    let coordinator_attempt = AttemptIdentity::new(
        snapshot.world.world().clone(),
        TaskId::new("project-coordinator-attempt").unwrap(),
        AttemptGeneration::new(1).unwrap(),
    );
    let launch = HostedWorldLaunchV1::new(
        &logical,
        &deployment,
        &snapshot,
        governor,
        coordinator_observer(11, 12, 13),
        coordinator_attempt,
        ReceiptIdentity::new(
            snapshot.world.world().clone(),
            ReceiptId::new("project-world-runtime").unwrap(),
        ),
        &attempts,
    )
    .unwrap();
    let current = HostedWorldCurrentV1::from_launch(&launch).unwrap();
    let signer = Ed25519ReceiptSigner::from_secret_bytes(TEST_RECEIPT_SECRET);
    WorldFixture {
        bundle,
        project,
        snapshot,
        deployment,
        launch,
        current,
        signer,
    }
}

fn execute(fixture: &WorldFixture) -> anyhow::Result<project::WorldProjectExecutionOutcome> {
    execute_world_project_with_receipt(
        &fixture.bundle,
        &fixture.project,
        &RunOptions::default(),
        &fixture.deployment,
        &fixture.snapshot,
        &fixture.launch,
        &fixture.current,
        &fixture.signer,
    )
}

fn assert_runtime_graph_rejected(graph: &RuntimeGraphV1, expected: &str) {
    let validation = graph.validate().unwrap_err();
    assert!(
        validation.to_string().contains(expected),
        "unexpected RuntimeGraph validation error: {validation}"
    );

    // Serialize directly because canonical_bytes() correctly refuses to encode
    // an invalid graph. serde's struct-field order is the canonical candidate
    // that an external decoder would otherwise be asked to accept.
    let encoded = serde_json::to_vec(graph).unwrap();
    let decoding = RuntimeGraphV1::decode_canonical(&encoded).unwrap_err();
    assert!(
        decoding.to_string().contains(expected),
        "unexpected canonical RuntimeGraph decode error: {decoding}"
    );
}

fn write_optional_native_fixture(outcome: &project::WorldProjectExecutionOutcome) {
    if let Some(path) = std::env::var_os("O_PROJECT_WORLD_RECEIPT_HEX_OUT") {
        write_world_project_receipt_hex(Path::new(&path), outcome).unwrap();
    }
    if let Some(path) = std::env::var_os("O_PROJECT_WORLD_RECEIPT_SEMANTIC_OUT") {
        fs::write(
            Path::new(&path),
            format!("{}\n", hex::encode(outcome.receipt_semantic_sha256)),
        )
        .unwrap();
    }
}

#[test]
fn world_bound_success_observes_runtime_graph_and_emits_uncommitted_receipt() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("executed.marker");
    let fixture = fixture(&marker, RouteMode::Success);
    let outcome = execute(&fixture).unwrap();

    assert!(
        outcome.coordinator_succeeded(),
        "unexpected coordinator failure: {:?}",
        outcome.coordinator_failure
    );
    let result = outcome.result.as_ref().unwrap();
    assert_eq!(result.route_id, "main");
    assert!(result.succeeded());
    let marker_text = fs::read_to_string(&marker).unwrap();
    assert_eq!(marker_text, "prepare-executed\nmain-executed\n");

    let header = outcome.trace.header();
    assert_eq!(
        header.deployment_plan_digest,
        fixture.deployment.digest().unwrap().as_sha256()
    );
    assert_eq!(
        header.execution_attempt_id,
        fixture.launch.coordinator_attempt().to_string()
    );
    assert_eq!(
        outcome.runtime_graph.deployment_plan,
        fixture.deployment.digest().unwrap()
    );
    assert_eq!(
        outcome.runtime_graph.trace_execution_attempt_id,
        fixture.launch.coordinator_attempt().to_string()
    );
    assert_eq!(outcome.runtime_graph.world, *fixture.launch.world());

    let canonical_graph = outcome.runtime_graph.canonical_bytes().unwrap();
    assert_eq!(
        RuntimeGraphV1::decode_canonical(&canonical_graph).unwrap(),
        outcome.runtime_graph
    );
    outcome
        .runtime_graph
        .validate_trusted_project_result(
            &fixture.project,
            &fixture.deployment,
            &fixture.launch,
            &outcome.trace,
            result,
        )
        .unwrap();
    assert!(matches!(
        &outcome.runtime_graph.terminal,
        RuntimeGraphTerminalV1::RouteSettlement {
            residual_host_world: true,
            ..
        }
    ));

    let resolver = ExactResolver {
        key_id: fixture.signer.key_id(),
        public: fixture.signer.public_key_bytes(),
    };
    let verified = outcome.signed_receipt.clone().verify(&resolver).unwrap();
    assert_eq!(verified.signed().bytes(), outcome.signed_receipt.bytes());
    assert_eq!(
        verified.receipt().context().attempt(),
        fixture.launch.coordinator_attempt()
    );
    assert_eq!(
        verified.receipt().context().placement().node(),
        &fixture.launch.coordinator_observer().node
    );
    assert_ne!(
        verified.receipt().context().placement().node(),
        &fixture.launch.selected_provider().node,
        "receipt observation context must not masquerade as proposed provider placement"
    );
    assert!(verified.receipt().subject().package().is_none());
    assert!(matches!(
        verified.receipt().commit(),
        ReceiptCommitFenceV1::Uncommitted
    ));
    assert!(matches!(
        verified.receipt().terminal(),
        ReceiptTerminalV1::Success(_)
    ));
    assert_eq!(
        project_receipt_semantic_sha256_v1(outcome.signed_receipt.bytes()).unwrap(),
        outcome.receipt_semantic_sha256
    );

    // Canonical encoding and trusted-input substitutions remain separate
    // gates: either one must fail closed when its own contract is violated.
    let pretty = serde_json::to_vec_pretty(&outcome.runtime_graph).unwrap();
    assert!(RuntimeGraphV1::decode_canonical(&pretty).is_err());
    let mut substituted = outcome.runtime_graph.clone();
    substituted.project_bundle = artifact("substituted-project-bundle");
    substituted.validate().unwrap();
    assert!(substituted
        .validate_trusted_project_result(
            &fixture.project,
            &fixture.deployment,
            &fixture.launch,
            &outcome.trace,
            result,
        )
        .is_err());

    // A terminal copied from a prerequisite is not the route selected at the
    // completed root. Both standalone decoding and trusted replay reject it.
    let (prepare_operation, prepare_residual, prepare_observation) = outcome
        .runtime_graph
        .operations
        .iter()
        .find_map(|operation| {
            operation
                .observations
                .last()
                .filter(|observation| {
                    observation.route_id.as_deref() == Some("prepare")
                        && observation.outcome.is_some()
                })
                .map(|observation| {
                    (
                        operation.logical_operation,
                        operation.residual_host_world,
                        observation.clone(),
                    )
                })
        })
        .unwrap();
    let mut terminal_substitution = outcome.runtime_graph.clone();
    terminal_substitution.terminal = RuntimeGraphTerminalV1::RouteSettlement {
        selected_operation: prepare_operation,
        route_id: "prepare".to_owned(),
        disposition: if prepare_observation.state == ProjectAttemptState::Skipped {
            project::RouteExecutionDisposition::GuardSkipped
        } else {
            project::RouteExecutionDisposition::Executed
        },
        settlement: prepare_observation.state,
        outcome: prepare_observation.outcome.unwrap(),
        residual_host_world: prepare_residual,
    };
    terminal_substitution.policy = "default".to_owned();
    for observation in &mut terminal_substitution
        .operations
        .last_mut()
        .unwrap()
        .observations
    {
        observation.operation_label = "select-route:default".to_owned();
    }
    assert_runtime_graph_rejected(&terminal_substitution, "policy-selected top-level route");
    assert!(terminal_substitution
        .validate_trusted_project_result(
            &fixture.project,
            &fixture.deployment,
            &fixture.launch,
            &outcome.trace,
            result,
        )
        .is_err());

    // Standalone structural evidence may not alias World task/attempt identity.
    let mut duplicated_identity = outcome.runtime_graph.clone();
    let duplicate_task = duplicated_identity.operations[0].task.clone();
    let duplicate_attempt = duplicated_identity.operations[0].attempt.clone();
    duplicated_identity.operations[1].task = duplicate_task.clone();
    duplicated_identity.operations[1].attempt = duplicate_attempt.clone();
    for observation in &mut duplicated_identity.operations[1].observations {
        observation.task = duplicate_task.clone();
        observation.attempt = duplicate_attempt.clone();
    }
    assert!(duplicated_identity.validate().is_err());

    // A lifecycle-valid trace that omits a prerequisite is causally invalid.
    let mut omitted_prerequisite = outcome
        .trace
        .events()
        .iter()
        .filter(|event| event.route_id.as_deref() != Some("prepare"))
        .cloned()
        .collect::<Vec<_>>();
    for (ordinal, event) in omitted_prerequisite.iter_mut().enumerate() {
        event.coordinator_ordinal = u64::try_from(ordinal).unwrap();
    }
    let structural_trace =
        ProjectAttemptTrace::try_from_events(outcome.trace.header().clone(), omitted_prerequisite)
            .unwrap();
    assert!(RuntimeGraphV1::from_project_result(
        &fixture.project,
        &fixture.deployment,
        &fixture.launch,
        &structural_trace,
        result,
    )
    .is_err());
    assert!(RuntimeGraphV1::from_coordinator_failure(
        &fixture.project,
        &fixture.deployment,
        &fixture.launch,
        &outcome.trace,
        b"forged failure after completed selection",
    )
    .is_err());
    let mut signature_tamper = outcome.signed_receipt.bytes().to_vec();
    *signature_tamper.last_mut().unwrap() ^= 1;
    assert_eq!(
        project_receipt_semantic_sha256_v1(&signature_tamper).unwrap(),
        outcome.receipt_semantic_sha256,
        "the semantic comparison deliberately excludes the signature envelope"
    );
    assert!(o_lang::world::inspect_signed_receipt_v1(&signature_tamper)
        .unwrap()
        .verify(&resolver)
        .is_err());

    write_optional_native_fixture(&outcome);
}

#[test]
fn world_bound_route_execution_budget_fails_before_launch_or_receipt() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("world-budget-must-not-launch.marker");
    let fixture = fixture(&marker, RouteMode::Success);
    let mut options = RunOptions::default();
    options.limits.max_routes_per_selection = 1;

    let error = execute_world_project_with_receipt(
        &fixture.bundle,
        &fixture.project,
        &options,
        &fixture.deployment,
        &fixture.snapshot,
        &fixture.launch,
        &fixture.current,
        &fixture.signer,
    )
    .unwrap_err();

    assert!(matches!(
        error.downcast_ref::<RouteExecutionError>(),
        Some(RouteExecutionError::Configuration { detail })
            if detail.contains("could execute 2 routes including prerequisites")
    ));
    assert!(!marker.exists(), "World budget failure launched a route");
}

#[test]
fn runtime_graph_decode_rejects_constructor_impossible_terminal_shapes() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("runtime-graph-negative.marker");
    let fixture = fixture(&marker, RouteMode::Success);
    let outcome = execute(&fixture).unwrap();
    assert!(
        outcome.coordinator_succeeded(),
        "unexpected coordinator failure: {:?}",
        outcome.coordinator_failure
    );
    let valid = outcome.runtime_graph;
    valid.validate().unwrap();

    let residual_host_world = match &valid.terminal {
        RuntimeGraphTerminalV1::RouteSettlement {
            residual_host_world,
            ..
        } => *residual_host_world,
        RuntimeGraphTerminalV1::CoordinatorFailure { .. } => unreachable!(),
    };

    let mut no_started = valid.clone();
    for operation in &mut no_started.operations {
        operation.observations.clear();
    }
    no_started.terminal = RuntimeGraphTerminalV1::CoordinatorFailure {
        detail_sha256: artifact("coordinator-failed-before-start"),
        residual_host_world: false,
    };
    assert_runtime_graph_rejected(&no_started, "no observed started operation");

    let mut failure_after_completed_root = valid.clone();
    failure_after_completed_root.terminal = RuntimeGraphTerminalV1::CoordinatorFailure {
        detail_sha256: artifact("failure-after-completed-root"),
        residual_host_world,
    };
    assert_runtime_graph_rejected(&failure_after_completed_root, "completed SelectRoute root");

    let mut completed_after_abort = valid.clone();
    let aborted = completed_after_abort
        .operations
        .iter_mut()
        .flat_map(|operation| operation.observations.iter_mut())
        .find(|observation| {
            observation.route_id.as_deref() == Some("prepare")
                && observation.state == ProjectAttemptState::SettledSuccess
        })
        .unwrap();
    aborted.state = ProjectAttemptState::Aborted;
    aborted.failure_sha256 = Some("a".repeat(64));
    assert_runtime_graph_rejected(&completed_after_abort, "contains an aborted operation");

    let mut completed_after_failed_prerequisite = valid.clone();
    let failed_prerequisite = completed_after_failed_prerequisite
        .operations
        .iter_mut()
        .flat_map(|operation| operation.observations.iter_mut())
        .find(|observation| {
            observation.route_id.as_deref() == Some("prepare")
                && observation.state == ProjectAttemptState::SettledSuccess
        })
        .unwrap();
    failed_prerequisite.state = ProjectAttemptState::SettledFailure;
    failed_prerequisite.outcome.as_mut().unwrap().exit_code = Some(7);
    assert_runtime_graph_rejected(
        &completed_after_failed_prerequisite,
        "completes selection after a failed prerequisite",
    );

    let mut settlement_without_root = valid.clone();
    settlement_without_root
        .operations
        .last_mut()
        .unwrap()
        .observations
        .clear();
    assert_runtime_graph_rejected(&settlement_without_root, "completed SelectRoute root");

    let mut inconsistent_residual = valid.clone();
    match &mut inconsistent_residual.terminal {
        RuntimeGraphTerminalV1::RouteSettlement {
            residual_host_world,
            ..
        }
        | RuntimeGraphTerminalV1::CoordinatorFailure {
            residual_host_world,
            ..
        } => *residual_host_world = !*residual_host_world,
    }
    assert_runtime_graph_rejected(&inconsistent_residual, "residual HostWorld truth");

    let mut impossible_lifecycle = valid.clone();
    impossible_lifecycle
        .operations
        .iter_mut()
        .find(|operation| !operation.observations.is_empty())
        .unwrap()
        .observations[0]
        .state = ProjectAttemptState::Started;
    assert_runtime_graph_rejected(&impossible_lifecycle, "invalid project attempt transition");

    let mut noncanonical_policy = valid.clone();
    noncanonical_policy.policy = "any".to_owned();
    assert_runtime_graph_rejected(&noncanonical_policy, "canonical resolved policy token");

    let mut noncanonical_branch = valid.clone();
    for observation in noncanonical_branch
        .operations
        .iter_mut()
        .flat_map(|operation| operation.observations.iter_mut())
    {
        if observation.branch.is_some() {
            observation.branch = Some(99);
        }
    }
    assert_runtime_graph_rejected(&noncanonical_branch, "uses a nonzero branch");

    let make_two_branch_any_success = |graph: &mut RuntimeGraphV1| {
        graph.policy = "any_success".to_owned();
        for observation in graph
            .operations
            .iter_mut()
            .flat_map(|operation| operation.observations.iter_mut())
        {
            if observation.route_id.as_deref() == Some("main")
                && observation.operation_label == "run-route:main"
            {
                observation.branch = Some(1);
            }
        }
        for observation in &mut graph.operations.last_mut().unwrap().observations {
            observation.operation_label = "select-route:any_success".to_owned();
        }
    };

    let mut later_branch_after_success = valid.clone();
    make_two_branch_any_success(&mut later_branch_after_success);
    let (prepare_operation, prepare_residual, prepare_observation) = later_branch_after_success
        .operations
        .iter()
        .find_map(|operation| {
            operation
                .observations
                .last()
                .filter(|observation| {
                    observation.route_id.as_deref() == Some("prepare")
                        && observation.state == ProjectAttemptState::SettledSuccess
                })
                .map(|observation| {
                    (
                        operation.logical_operation,
                        operation.residual_host_world,
                        observation.clone(),
                    )
                })
        })
        .unwrap();
    later_branch_after_success.terminal = RuntimeGraphTerminalV1::RouteSettlement {
        selected_operation: prepare_operation,
        route_id: "prepare".to_owned(),
        disposition: project::RouteExecutionDisposition::Executed,
        settlement: ProjectAttemptState::SettledSuccess,
        outcome: prepare_observation.outcome.unwrap(),
        residual_host_world: prepare_residual,
    };
    assert_runtime_graph_rejected(
        &later_branch_after_success,
        "ordered RuntimeGraph observes a later branch after prior success",
    );

    let mut later_branch_without_admission = valid.clone();
    make_two_branch_any_success(&mut later_branch_without_admission);
    let prepare = later_branch_without_admission
        .operations
        .iter_mut()
        .flat_map(|operation| operation.observations.iter_mut())
        .find(|observation| {
            observation.route_id.as_deref() == Some("prepare")
                && observation.state == ProjectAttemptState::SettledSuccess
        })
        .unwrap();
    prepare.state = ProjectAttemptState::SettledFailure;
    prepare.outcome.as_mut().unwrap().exit_code = Some(7);
    assert_runtime_graph_rejected(
        &later_branch_without_admission,
        "ordered RuntimeGraph later branch lacks an admitted prior continuation",
    );

    let mut completed_with_unfollowed_admission = valid.clone();
    completed_with_unfollowed_admission.policy = "any_success".to_owned();
    for observation in &mut completed_with_unfollowed_admission
        .operations
        .last_mut()
        .unwrap()
        .observations
    {
        observation.operation_label = "select-route:any_success".to_owned();
    }
    let selected = completed_with_unfollowed_admission
        .operations
        .iter_mut()
        .flat_map(|operation| operation.observations.iter_mut())
        .find(|observation| {
            observation.route_id.as_deref() == Some("main")
                && observation.state == ProjectAttemptState::SettledSuccess
        })
        .unwrap();
    selected.state = ProjectAttemptState::SettledFailure;
    selected.outcome.as_mut().unwrap().exit_code = Some(7);
    selected.continuation = Some(
        ProjectContinuationDecision::new(
            "never-ran",
            vec!["prepare".to_owned(), "main".to_owned()],
            ProjectContinuationEvidence::DeclaredIdempotent,
        )
        .unwrap(),
    );
    let selected_outcome = selected.outcome.clone().unwrap();
    match &mut completed_with_unfollowed_admission.terminal {
        RuntimeGraphTerminalV1::RouteSettlement {
            settlement,
            outcome,
            ..
        } => {
            *settlement = ProjectAttemptState::SettledFailure;
            *outcome = selected_outcome;
        }
        RuntimeGraphTerminalV1::CoordinatorFailure { .. } => unreachable!(),
    }
    assert_runtime_graph_rejected(
        &completed_with_unfollowed_admission,
        "stops after admitting continuation",
    );

    let mut admitted_partial_final_branch = valid.clone();
    admitted_partial_final_branch.policy = "any_success".to_owned();
    admitted_partial_final_branch
        .operations
        .last_mut()
        .unwrap()
        .observations
        .clear();
    let prepare = admitted_partial_final_branch
        .operations
        .iter_mut()
        .flat_map(|operation| operation.observations.iter_mut())
        .find(|observation| {
            observation.route_id.as_deref() == Some("prepare")
                && observation.state == ProjectAttemptState::SettledSuccess
        })
        .unwrap();
    prepare.state = ProjectAttemptState::SettledFailure;
    prepare.outcome.as_mut().unwrap().exit_code = Some(7);
    prepare.continuation = Some(
        ProjectContinuationDecision::new(
            "main2",
            vec!["prepare".to_owned()],
            ProjectContinuationEvidence::DeclaredIdempotent,
        )
        .unwrap(),
    );
    let partial_route = admitted_partial_final_branch
        .operations
        .iter_mut()
        .find(|operation| {
            operation
                .observations
                .first()
                .is_some_and(|observation| observation.operation_label == "run-route:main")
        })
        .unwrap();
    partial_route.observations.pop();
    for observation in &mut partial_route.observations {
        observation.operation_label = "run-route:prep2".to_owned();
        observation.route_id = Some("prep2".to_owned());
        observation.branch = Some(1);
    }
    let mut ordinals = admitted_partial_final_branch
        .operations
        .iter()
        .enumerate()
        .flat_map(|(operation, value)| {
            value
                .observations
                .iter()
                .enumerate()
                .map(move |(observation, value)| {
                    (value.coordinator_ordinal, operation, observation)
                })
        })
        .collect::<Vec<_>>();
    ordinals.sort_unstable();
    for (ordinal, (_, operation, observation)) in ordinals.into_iter().enumerate() {
        admitted_partial_final_branch.operations[operation].observations[observation]
            .coordinator_ordinal = u64::try_from(ordinal).unwrap();
    }
    admitted_partial_final_branch.terminal = RuntimeGraphTerminalV1::CoordinatorFailure {
        detail_sha256: artifact("admitted-partial-final-branch"),
        residual_host_world: true,
    };
    admitted_partial_final_branch.validate().unwrap();
    assert_eq!(
        RuntimeGraphV1::decode_canonical(&admitted_partial_final_branch.canonical_bytes().unwrap())
            .unwrap(),
        admitted_partial_final_branch
    );

    let mut incomplete_success = valid.clone();
    let selected_outcome = incomplete_success
        .operations
        .iter_mut()
        .flat_map(|operation| operation.observations.iter_mut())
        .find(|observation| {
            observation.route_id.as_deref() == Some("main") && observation.outcome.is_some()
        })
        .and_then(|observation| observation.outcome.as_mut())
        .unwrap();
    selected_outcome.artifact_capture = ArtifactCaptureStatus::Incomplete {
        failure: Box::new(ArtifactCaptureFailure::Missing {
            requirement: "forged-missing.bin".to_owned(),
        }),
    };
    selected_outcome.artifacts.clear();
    assert_runtime_graph_rejected(
        &incomplete_success,
        "exit-zero route outcome has incomplete artifact evidence",
    );

    for invalid_path in ["../escape.bin", "/absolute.bin", "nested//file.bin"] {
        let mut invalid_artifact_path = valid.clone();
        let observation_outcome = invalid_artifact_path
            .operations
            .iter_mut()
            .flat_map(|operation| operation.observations.iter_mut())
            .find(|observation| {
                observation.route_id.as_deref() == Some("main") && observation.outcome.is_some()
            })
            .and_then(|observation| observation.outcome.as_mut())
            .unwrap();
        observation_outcome.artifacts[0].path = invalid_path.to_owned();
        match &mut invalid_artifact_path.terminal {
            RuntimeGraphTerminalV1::RouteSettlement { outcome, .. } => {
                outcome.artifacts[0].path = invalid_path.to_owned();
            }
            RuntimeGraphTerminalV1::CoordinatorFailure { .. } => unreachable!(),
        }
        assert_runtime_graph_rejected(
            &invalid_artifact_path,
            "artifact path must be a canonical relative workspace path",
        );
    }

    let mut impossible_empty_stream = valid.clone();
    let empty_stream = impossible_empty_stream
        .operations
        .iter_mut()
        .flat_map(|operation| operation.observations.iter_mut())
        .filter_map(|observation| observation.outcome.as_mut())
        .find(|outcome| outcome.stderr_total_observed_bytes == 0)
        .unwrap();
    empty_stream.stderr_sha256 = "0".repeat(64);
    assert_runtime_graph_rejected(
        &impossible_empty_stream,
        "empty stderr stream has a nonempty-content fingerprint",
    );

    let mut forged_route_identity = valid;
    let selected_route = forged_route_identity
        .operations
        .iter_mut()
        .find(|operation| {
            operation
                .observations
                .last()
                .is_some_and(|observation| observation.route_id.as_deref() == Some("main"))
        })
        .unwrap();
    for observation in &mut selected_route.observations {
        observation.operation_label = "run-route:forged-main".to_owned();
    }
    assert_runtime_graph_rejected(&forged_route_identity, "noncanonical operation label");
}

#[test]
fn nonzero_route_settlement_is_not_mislabeled_as_receipt_success() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("nonzero-execution.marker");
    let fixture = fixture(&marker, RouteMode::NonZero);
    let outcome = execute(&fixture).unwrap();

    assert!(outcome.coordinator_succeeded());
    assert!(!outcome.route_succeeded());
    assert_eq!(outcome.result.as_ref().unwrap().exit_code, Some(9));
    assert!(matches!(
        outcome.runtime_graph.terminal,
        RuntimeGraphTerminalV1::RouteSettlement {
            settlement: ProjectAttemptState::SettledFailure,
            ..
        }
    ));
    let resolver = ExactResolver {
        key_id: fixture.signer.key_id(),
        public: fixture.signer.public_key_bytes(),
    };
    let verified = outcome.signed_receipt.clone().verify(&resolver).unwrap();
    assert!(matches!(
        verified.receipt().terminal(),
        ReceiptTerminalV1::Failure { code, .. }
            if code == "project-route-settled-failure"
    ));
    assert!(matches!(
        verified.receipt().commit(),
        ReceiptCommitFenceV1::Uncommitted
    ));
}

#[test]
fn guard_skip_has_distinct_failure_evidence_without_launching_selected_route() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("guard-skip.marker");
    let fixture = fixture(&marker, RouteMode::GuardSkipped);
    let opts = RunOptions {
        guard_behavior: GuardBehavior::Skip,
        ..RunOptions::default()
    };
    let outcome = execute_world_project_with_receipt(
        &fixture.bundle,
        &fixture.project,
        &opts,
        &fixture.deployment,
        &fixture.snapshot,
        &fixture.launch,
        &fixture.current,
        &fixture.signer,
    )
    .unwrap();

    assert!(
        outcome.coordinator_succeeded(),
        "guard-skip coordinator failure: {:?}",
        outcome.coordinator_failure
    );
    assert!(!outcome.route_succeeded());
    assert!(outcome.result.as_ref().unwrap().was_guard_skipped());
    assert_eq!(fs::read_to_string(&marker).unwrap(), "prepare-executed\n");
    assert!(matches!(
        &outcome.runtime_graph.terminal,
        RuntimeGraphTerminalV1::RouteSettlement {
            disposition: project::RouteExecutionDisposition::GuardSkipped,
            settlement: ProjectAttemptState::Skipped,
            ..
        }
    ));

    let mut impossible_capture = outcome.runtime_graph.clone();
    let mut forged_fingerprint = impossible_capture
        .operations
        .iter()
        .flat_map(|operation| operation.observations.iter())
        .filter_map(|observation| observation.outcome.as_ref())
        .flat_map(|route_outcome| route_outcome.artifacts.iter())
        .next()
        .unwrap()
        .clone();
    forged_fingerprint.path = "result.txt".to_owned();
    let skipped = impossible_capture
        .operations
        .iter_mut()
        .flat_map(|operation| operation.observations.iter_mut())
        .find(|observation| observation.state == ProjectAttemptState::Skipped)
        .and_then(|observation| observation.outcome.as_mut())
        .unwrap();
    skipped.artifacts = vec![forged_fingerprint.clone()];
    skipped.artifact_capture = ArtifactCaptureStatus::Complete;
    match &mut impossible_capture.terminal {
        RuntimeGraphTerminalV1::RouteSettlement { outcome, .. } => {
            outcome.artifacts = vec![forged_fingerprint];
            outcome.artifact_capture = ArtifactCaptureStatus::Complete;
        }
        RuntimeGraphTerminalV1::CoordinatorFailure { .. } => unreachable!(),
    }
    assert_runtime_graph_rejected(
        &impossible_capture,
        "skipped route settlement carries captured artifacts",
    );

    let mut impossible_stdout = outcome.runtime_graph.clone();
    let skipped = impossible_stdout
        .operations
        .iter_mut()
        .flat_map(|operation| operation.observations.iter_mut())
        .find(|observation| observation.state == ProjectAttemptState::Skipped)
        .and_then(|observation| observation.outcome.as_mut())
        .unwrap();
    skipped.stdout_sha256 = hex::encode(Sha256::digest(b"forged"));
    skipped.stdout_total_observed_bytes = 6;
    skipped.stdout_retained_bytes = 6;
    match &mut impossible_stdout.terminal {
        RuntimeGraphTerminalV1::RouteSettlement { outcome, .. } => {
            outcome.stdout_sha256 = hex::encode(Sha256::digest(b"forged"));
            outcome.stdout_total_observed_bytes = 6;
            outcome.stdout_retained_bytes = 6;
        }
        RuntimeGraphTerminalV1::CoordinatorFailure { .. } => unreachable!(),
    }
    assert_runtime_graph_rejected(
        &impossible_stdout,
        "skipped route settlement must have an empty complete stdout stream",
    );

    let resolver = ExactResolver {
        key_id: fixture.signer.key_id(),
        public: fixture.signer.public_key_bytes(),
    };
    let verified = outcome.signed_receipt.clone().verify(&resolver).unwrap();
    assert!(matches!(
        verified.receipt().terminal(),
        ReceiptTerminalV1::Failure { code, .. }
            if code == "project-route-guard-skipped"
    ));
    assert!(matches!(
        verified.receipt().commit(),
        ReceiptCommitFenceV1::Uncommitted
    ));
}

#[test]
fn stale_world_and_every_provider_generation_fail_before_execution() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("must-not-execute.marker");
    let fixture = fixture(&marker, RouteMode::Success);
    let implementation = fixture.current.selected_provider.implementation.clone();
    let mut variants = Vec::new();

    let mut stale_world = fixture.current.clone();
    stale_world.world = world(8);
    stale_world.governor = GovernorIdentity::new(
        stale_world.world.clone(),
        fixture.current.governor.term(),
        fixture.current.governor.log_index(),
    );
    variants.push(("World epoch", stale_world));

    let mut stale_governor = fixture.current.clone();
    stale_governor.governor = GovernorIdentity::new(
        stale_governor.world.clone(),
        GovernorTerm::new(fixture.current.governor.term().get() + 1).unwrap(),
        fixture.current.governor.log_index(),
    );
    variants.push(("Governor term", stale_governor));

    for (label, generations) in [
        ("provider node", (3, 3, 4, 5)),
        ("provider domain", (2, 4, 4, 5)),
        ("provider process", (2, 3, 5, 5)),
        ("provider service", (2, 3, 4, 6)),
    ] {
        let mut current = fixture.current.clone();
        current.selected_provider = provider_binding(
            generations.0,
            generations.1,
            generations.2,
            generations.3,
            implementation.clone(),
        );
        variants.push((label, current));
    }

    for (label, generations) in [
        ("coordinator observer node", (12, 12, 13)),
        ("coordinator observer domain", (11, 13, 13)),
        ("coordinator observer process", (11, 12, 14)),
    ] {
        let mut current = fixture.current.clone();
        current.coordinator_observer =
            coordinator_observer(generations.0, generations.1, generations.2);
        variants.push((label, current));
    }

    let mut stale_coordinator_attempt = fixture.current.clone();
    stale_coordinator_attempt.coordinator_attempt = AttemptIdentity::new(
        stale_coordinator_attempt
            .coordinator_attempt
            .world()
            .clone(),
        stale_coordinator_attempt.coordinator_attempt.task().clone(),
        AttemptGeneration::new(
            stale_coordinator_attempt
                .coordinator_attempt
                .attempt()
                .get()
                + 1,
        )
        .unwrap(),
    );
    variants.push(("coordinator attempt", stale_coordinator_attempt));

    let mut stale_attempt = fixture.current.clone();
    let operation_attempt = &mut stale_attempt.operation_attempts[0];
    operation_attempt.attempt = AttemptIdentity::new(
        operation_attempt.attempt.world().clone(),
        operation_attempt.attempt.task().clone(),
        AttemptGeneration::new(operation_attempt.attempt.attempt().get() + 1).unwrap(),
    );
    variants.push(("operation attempt", stale_attempt));

    let mut changed_implementation = fixture.current.clone();
    changed_implementation.selected_provider.implementation = artifact("replacement-provider");
    variants.push(("provider implementation", changed_implementation));

    for (label, current) in variants {
        let result = execute_world_project_with_receipt(
            &fixture.bundle,
            &fixture.project,
            &RunOptions::default(),
            &fixture.deployment,
            &fixture.snapshot,
            &fixture.launch,
            &current,
            &fixture.signer,
        );
        assert!(
            result.is_err(),
            "{label} substitution unexpectedly executed"
        );
        assert!(
            !marker.exists(),
            "{label} substitution reached route execution before rejection"
        );
    }
}

#[test]
fn coordinator_abort_after_start_emits_failure_graph_and_uncommitted_receipt() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("partial-execution.marker");
    let fixture = fixture(&marker, RouteMode::SpawnAbort);
    let outcome = execute(&fixture).unwrap();

    assert!(!outcome.coordinator_succeeded());
    assert!(outcome.result.is_none());
    assert!(outcome.coordinator_failure.is_some());
    assert_eq!(fs::read_to_string(&marker).unwrap(), "prepare-executed\n");
    assert!(outcome
        .trace
        .events()
        .iter()
        .any(|event| event.state == ProjectAttemptState::Aborted));
    assert!(matches!(
        outcome.runtime_graph.terminal,
        RuntimeGraphTerminalV1::CoordinatorFailure {
            residual_host_world: true,
            ..
        }
    ));
    outcome
        .runtime_graph
        .validate_trusted_coordinator_failure(
            &fixture.project,
            &fixture.deployment,
            &fixture.launch,
            &outcome.trace,
            outcome.coordinator_failure.as_ref().unwrap().as_bytes(),
        )
        .unwrap();

    let resolver = ExactResolver {
        key_id: fixture.signer.key_id(),
        public: fixture.signer.public_key_bytes(),
    };
    let verified = outcome.signed_receipt.clone().verify(&resolver).unwrap();
    assert!(matches!(
        verified.receipt().commit(),
        ReceiptCommitFenceV1::Uncommitted
    ));
    assert!(matches!(
        verified.receipt().terminal(),
        ReceiptTerminalV1::Failure { .. }
    ));
    assert_eq!(
        project_receipt_semantic_sha256_v1(outcome.signed_receipt.bytes()).unwrap(),
        outcome.receipt_semantic_sha256
    );
}

#[test]
fn incomplete_required_artifact_cannot_emit_a_success_receipt() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("incomplete-artifact.marker");
    let fixture = fixture(&marker, RouteMode::MissingArtifact);
    let outcome = execute(&fixture).unwrap();

    assert!(!outcome.coordinator_succeeded());
    assert!(outcome.result.is_none());
    assert!(
        outcome
            .coordinator_failure
            .as_deref()
            .is_some_and(|failure| failure
                .contains("declared artifact `required-but-not-produced.bin` is missing")),
        "unexpected coordinator failure: {:?}",
        outcome.coordinator_failure
    );
    assert!(outcome.trace.events().iter().any(|event| {
        event.operation_label == "run-route:main" && event.state == ProjectAttemptState::Aborted
    }));
    assert!(!outcome.trace.events().iter().any(|event| {
        event.operation_label == "run-route:main"
            && event.state == ProjectAttemptState::SettledSuccess
    }));
    assert!(matches!(
        outcome.runtime_graph.terminal,
        RuntimeGraphTerminalV1::CoordinatorFailure {
            residual_host_world: true,
            ..
        }
    ));

    let resolver = ExactResolver {
        key_id: fixture.signer.key_id(),
        public: fixture.signer.public_key_bytes(),
    };
    let verified = outcome.signed_receipt.clone().verify(&resolver).unwrap();
    assert!(matches!(
        verified.receipt().terminal(),
        ReceiptTerminalV1::Failure { code, .. }
            if code == "project-coordinator-failure"
    ));
    assert!(matches!(
        verified.receipt().commit(),
        ReceiptCommitFenceV1::Uncommitted
    ));
}
