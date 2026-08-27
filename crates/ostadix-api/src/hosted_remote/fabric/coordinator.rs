//! Explicit remote realization of the frozen M2 trusted-inline capsule.
//!
//! This module is the coordinator-side authority bridge. It derives a bounded
//! source closure only from an already admitted operation, signs the exact M2
//! capsule for one configured provider, and converts a remote result into an
//! ordinary worker completion only after the ordered Fabric acceptance gates.
//! No remote record contains a `TaskToken`, plan-node identity, or HGraph
//! coordinate.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};

use crate::backend_catalog::{BackendRegistry, ExecutionMode};
use crate::environment::EnvironmentRefV2;
use crate::eval::Evaluator;
use crate::eval_core::GraphEvalFrame;
use crate::evidence::{AdmittedExecution, ExecutionIntentV1};
use crate::execution_fabric::{
    encode_execution_capsule_v1, AttemptIdV1, CandidateOutcomeV1, ExecutionCapsuleV1,
    ExecutionIdV1, ExecutionLimitsV1, InputBindingV1, InputManifestV1, LogicalTaskIdV1,
    OutputContractV1, OutputValueKindV1, RendererPartV1, Sha256DigestV1, SourceClosedRendererV1,
};
use crate::execution_fabric_authority::{
    fabric_lease_sha256_v3, FabricAttemptQueryV1, FabricAttemptStatusV1, FabricRequestV1,
    FabricResponseV1, FabricSigningKeyV1, FabricSourceClosureV1, FabricSubmissionV1,
    FabricTargetBindingV1, FabricTerminalCandidateV1, PinnedFabricNodeKeyV1, PlacementLeaseV3,
    TrustedFabricAuthoritiesV1, FABRIC_SOURCE_CLOSURE_DIALECT_V1,
    FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1, MAX_FABRIC_LEASE_LIFETIME_MS,
};
use crate::hgraph::solve::solve_types;
use crate::hosted_remote::fabric::{
    trusted_inline_fabric_profile_v1, FabricAttemptClientV1, FabricClientFailureV1,
};
use crate::hosted_remote::{
    prepare_execution_fabric_client_tls_v1, ClientTlsIdentity, ExecutionFabricClientTlsV1,
};
use crate::ir::{ExecutionPlan, OIr, OIrProgram, PlanNodeId, PlanNodeKind};
use crate::parser::{escape_typed_body_literal, Parser};
use crate::placement_protocol::{SemanticDigestV1, UnixMillisV1};
use crate::value::OValue;
use crate::world::{PortableOValue, PortableValueRecord, MAX_OVALUE_RECORD_BYTES};

use crate::executor::pool::WorkerPool;
use crate::executor::task::{
    PhysicalAttemptCoordinateV1, PreparedTask, TaskSubmission, TaskToken, WorkerEvent,
};
use crate::executor::{AttemptDriver, PhysicalAttemptAdapterV1, PreparedPhysicalAttemptV1};

const DEFAULT_REMOTE_WORKER_CAPACITY_V1: usize = 1;
const DEFAULT_REMOTE_ATTEMPT_LIFETIME_MS_V1: u64 = 10_000;
const DEFAULT_REMOTE_MAX_RUNTIME_MS_V1: u64 = 5_000;
const DEFAULT_REMOTE_POLL_INTERVAL_MS_V1: u64 = 10;
const REMOTE_LOGICAL_TASK_DOMAIN_V1: &str = "ostadix/execution-fabric/logical-task/v1";
const REMOTE_LOGICAL_TASK_NONCE_DOMAIN_V1: &str = "ostadix/execution-fabric/logical-task-nonce/v1";
const REMOTE_LEASE_NONCE_DOMAIN_V1: &str = "ostadix/execution-fabric/lease-nonce/v1";
const REMOTE_RESULT_SLOT_V1: &str = "result";

/// Explicit physical-attempt selection for the narrow Fabric V1 profile.
///
/// This is additive execution policy over an already admitted V6 local-worker
/// renderer. It does not relabel admission's lane, authorize fallback, discover
/// a node, or choose placement. Every provider and authority coordinate is
/// supplied up front and retained immutably for the coordinator run.
#[derive(Clone)]
pub struct RemotePureExecutionConfigV1 {
    address: SocketAddr,
    tls: ExecutionFabricClientTlsV1,
    expected_server_principal_sha256: SemanticDigestV1,
    authority_signer: FabricSigningKeyV1,
    target: FabricTargetBindingV1,
    pinned_node: PinnedFabricNodeKeyV1,
    execution: ExecutionIdV1,
    worker_capacity: usize,
    connect_timeout: Duration,
    io_timeout: Duration,
    poll_interval: Duration,
    attempt_lifetime: Duration,
    limits: ExecutionLimitsV1,
}

impl std::fmt::Debug for RemotePureExecutionConfigV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemotePureExecutionConfigV1")
            .field("address", &self.address)
            .field("tls", &self.tls)
            .field(
                "expected_server_principal_sha256",
                &self.expected_server_principal_sha256,
            )
            .field("authority_signer", &"[redacted]")
            .field("target", &self.target)
            .field("pinned_node", &self.pinned_node)
            .field("execution", &self.execution)
            .field("worker_capacity", &self.worker_capacity)
            .field("connect_timeout", &self.connect_timeout)
            .field("io_timeout", &self.io_timeout)
            .field("poll_interval", &self.poll_interval)
            .field("attempt_lifetime", &self.attempt_lifetime)
            .field("limits", &self.limits)
            .finish()
    }
}

impl RemotePureExecutionConfigV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        address: impl Into<String>,
        tls_identity: ClientTlsIdentity,
        expected_server_principal_sha256: SemanticDigestV1,
        authority_signer: FabricSigningKeyV1,
        target: FabricTargetBindingV1,
        node_receipt_public_key: [u8; 32],
        execution: ExecutionIdV1,
    ) -> Result<Self> {
        let address_text = address.into();
        let address = address_text.parse::<SocketAddr>().with_context(|| {
            format!(
                "remote pure execution address `{address_text}` must be one exact numeric socket address"
            )
        })?;
        let tls = prepare_execution_fabric_client_tls_v1(&tls_identity)
            .context("remote pure execution failed to freeze its Fabric mTLS identity")?;
        let pinned_node = PinnedFabricNodeKeyV1::new(
            target.node_id(),
            target.node_generation(),
            target.execution_cell_incarnation(),
            node_receipt_public_key,
        )
        .map_err(anyhow::Error::new)?;
        let value = Self {
            address,
            tls,
            expected_server_principal_sha256,
            authority_signer,
            target,
            pinned_node,
            execution,
            worker_capacity: DEFAULT_REMOTE_WORKER_CAPACITY_V1,
            connect_timeout: crate::hosted_remote::DEFAULT_CONNECT_TIMEOUT,
            io_timeout: crate::hosted_remote::DEFAULT_IO_TIMEOUT,
            poll_interval: Duration::from_millis(DEFAULT_REMOTE_POLL_INTERVAL_MS_V1),
            attempt_lifetime: Duration::from_millis(DEFAULT_REMOTE_ATTEMPT_LIFETIME_MS_V1),
            limits: ExecutionLimitsV1::new(
                DEFAULT_REMOTE_MAX_RUNTIME_MS_V1,
                32 * 1024,
                MAX_OVALUE_RECORD_BYTES,
            )
            .map_err(anyhow::Error::new)?,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn with_worker_capacity(mut self, capacity: usize) -> Result<Self> {
        self.worker_capacity = capacity;
        self.validate()?;
        Ok(self)
    }

    pub fn with_timeouts(
        mut self,
        connect_timeout: Duration,
        io_timeout: Duration,
        poll_interval: Duration,
        attempt_lifetime: Duration,
    ) -> Result<Self> {
        self.connect_timeout = connect_timeout;
        self.io_timeout = io_timeout;
        self.poll_interval = poll_interval;
        self.attempt_lifetime = attempt_lifetime;
        self.validate()?;
        Ok(self)
    }

    pub fn with_limits(mut self, limits: ExecutionLimitsV1) -> Result<Self> {
        self.limits = limits;
        self.validate()?;
        Ok(self)
    }

    pub fn target(&self) -> &FabricTargetBindingV1 {
        &self.target
    }

    pub fn execution(&self) -> &ExecutionIdV1 {
        &self.execution
    }

    fn validate(&self) -> Result<()> {
        if self.worker_capacity == 0 {
            bail!("remote pure execution worker capacity must be nonzero");
        }
        if self.connect_timeout.is_zero()
            || self.io_timeout.is_zero()
            || self.poll_interval.is_zero()
            || self.attempt_lifetime.is_zero()
        {
            bail!("remote pure execution timeouts must all be nonzero");
        }
        let lifetime_ms = duration_millis(self.attempt_lifetime, "attempt lifetime")?;
        if lifetime_ms > MAX_FABRIC_LEASE_LIFETIME_MS {
            bail!(
                "remote pure execution attempt lifetime {lifetime_ms} ms exceeds Fabric V1 maximum {MAX_FABRIC_LEASE_LIFETIME_MS} ms"
            );
        }
        if self.limits.max_runtime_ms() > lifetime_ms {
            bail!("remote maximum runtime exceeds its signed attempt lifetime");
        }
        if self.target.node_id() != self.pinned_node.node_id()
            || self.target.node_generation() != self.pinned_node.node_generation()
            || self.target.execution_cell_incarnation()
                != self.pinned_node.execution_cell_incarnation()
        {
            bail!("remote target and pinned node receipt coordinates disagree");
        }
        Ok(())
    }

    fn client(&self) -> FabricAttemptClientV1 {
        FabricAttemptClientV1::from_frozen_tls(
            self.address,
            self.tls.clone(),
            self.expected_server_principal_sha256.as_sha256(),
            self.connect_timeout,
            self.io_timeout,
        )
    }
}

impl PhysicalAttemptAdapterV1 for RemotePureExecutionConfigV1 {
    fn create_driver(&self) -> Result<Box<dyn AttemptDriver>> {
        Ok(Box::new(RemotePureAttemptDriver::new(self)?))
    }

    fn prepare_attempt(
        &self,
        admitted: &AdmittedExecution<'_>,
        frame: &GraphEvalFrame,
        flat: &[&OIr],
        plan: &ExecutionPlan,
        id: PlanNodeId,
    ) -> Result<PreparedPhysicalAttemptV1> {
        let prepared = prepare_remote_pure_attempt_v1(self, admitted, frame, flat, plan, id)?;
        let coordinate = physical_attempt_coordinate_v1(prepared.attempt())?;
        Ok(PreparedPhysicalAttemptV1::new(
            coordinate,
            Box::new(prepared),
        ))
    }
}

impl Evaluator {
    /// Explicitly realize admitted trusted-inline renderer attempts through one
    /// exact authenticated Fabric target. This does not enable discovery,
    /// placement, retry, or local fallback.
    pub fn with_remote_pure_execution(self, config: RemotePureExecutionConfigV1) -> Self {
        self.with_physical_attempt_adapter(Arc::new(config))
    }
}

/// Driver over the unchanged five-method coordinator seam. Local scope reads
/// may still use its pool; only explicitly selected trusted-inline renderers
/// carry a remote attempt coordinate. There is no local renderer fallback.
pub(crate) struct RemotePureAttemptDriver {
    pool: WorkerPool,
    /// The coordinator-only identity bridge. `AttemptIdV1` is the remote
    /// protocol coordinate; `TaskToken` never crosses the Fabric wire.
    remote_attempts: Vec<RemoteAttemptBindingV1>,
    active_local: HashSet<TaskToken>,
    seen_local: HashSet<TaskToken>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteAttemptLifecycleV1 {
    Active,
    Fenced,
    Delivered,
}

struct RemoteAttemptBindingV1 {
    attempt: PhysicalAttemptCoordinateV1,
    token: TaskToken,
    lifecycle: RemoteAttemptLifecycleV1,
}

impl RemotePureAttemptDriver {
    pub(crate) fn new(config: &RemotePureExecutionConfigV1) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            pool: WorkerPool::new(config.worker_capacity)?,
            remote_attempts: Vec::new(),
            active_local: HashSet::new(),
            seen_local: HashSet::new(),
        })
    }

    fn plan_remote_submission(
        &self,
        token: TaskToken,
        attempt: PhysicalAttemptCoordinateV1,
    ) -> Result<Option<usize>> {
        if self.seen_local.contains(&token) {
            bail!(
                "remote attempt driver cannot rebind local task token {} to Fabric",
                token.0
            );
        }
        let mut highest_generation = None;
        let mut active_predecessor = None;
        for (index, binding) in self.remote_attempts.iter().enumerate() {
            if binding.attempt == attempt {
                bail!("remote attempt driver rejected reuse of a previously seen Fabric attempt");
            }
            let same_task = binding.attempt.same_logical_task(attempt);
            if binding.token == token && !same_task {
                bail!(
                    "remote attempt driver cannot bind task token {} to a different logical task",
                    token.0
                );
            }
            if !same_task {
                continue;
            }
            if binding.token != token {
                bail!("remote attempt driver cannot rebind a logical task to a different token");
            }
            highest_generation = Some(
                highest_generation
                    .unwrap_or(0_u64)
                    .max(binding.attempt.generation()),
            );
            match binding.lifecycle {
                RemoteAttemptLifecycleV1::Active => {
                    if active_predecessor.replace(index).is_some() {
                        bail!("remote attempt driver contains multiple active task generations");
                    }
                }
                RemoteAttemptLifecycleV1::Delivered => {
                    bail!("M3 remote attempt driver does not retry a delivered logical task");
                }
                RemoteAttemptLifecycleV1::Fenced => {}
            }
        }
        if highest_generation.is_some_and(|generation| attempt.generation() <= generation) {
            bail!("remote attempt driver rejected a stale attempt generation");
        }
        Ok(active_predecessor)
    }

    fn accept_event(&mut self, event: WorkerEvent) -> Result<Option<WorkerEvent>> {
        let WorkerEvent::Completion(completion) = &event else {
            return Ok(Some(event));
        };
        let token = completion.token;
        if let Some(attempt) = completion.physical_attempt() {
            let Some(binding) = self
                .remote_attempts
                .iter_mut()
                .find(|binding| binding.attempt == attempt)
            else {
                return Err(gate_error(
                    19,
                    "current attempt",
                    "completion names an unknown remote attempt",
                ));
            };
            if binding.token != token {
                return Err(gate_error(
                    19,
                    "current attempt",
                    "remote attempt maps to a different coordinator task token",
                ));
            }
            if binding.lifecycle != RemoteAttemptLifecycleV1::Active {
                crate::process::lifecycle_trace(
                    "fabric.candidate_discarded",
                    format!(
                        "token={} generation={} gate=19 state={:?}",
                        token.0,
                        attempt.generation(),
                        binding.lifecycle
                    ),
                );
                return Ok(None);
            }
            binding.lifecycle = RemoteAttemptLifecycleV1::Delivered;
            return Ok(Some(event));
        }
        if self.active_local.remove(&token) {
            return Ok(Some(event));
        }
        Err(gate_error(
            19,
            "current attempt",
            format!(
                "task token {} is fenced, superseded, already completed, or unknown",
                token.0
            ),
        ))
    }
}

impl AttemptDriver for RemotePureAttemptDriver {
    fn available_slots(&self) -> usize {
        self.pool.available_slots()
    }

    fn outstanding(&self) -> usize {
        self.pool.outstanding()
    }

    fn submit(&mut self, submission: TaskSubmission) -> Result<()> {
        let token = submission.token();
        let remote_attempt = submission.physical_attempt();
        if let Some(attempt) = &remote_attempt {
            let active_predecessor = self.plan_remote_submission(token, *attempt)?;
            self.remote_attempts
                .try_reserve(1)
                .context("remote attempt driver could not reserve identity-map capacity")?;
            self.pool.submit(submission)?;
            if let Some(index) = active_predecessor {
                self.remote_attempts[index].lifecycle = RemoteAttemptLifecycleV1::Fenced;
            }
            self.remote_attempts.push(RemoteAttemptBindingV1 {
                attempt: *attempt,
                token,
                lifecycle: RemoteAttemptLifecycleV1::Active,
            });
        } else {
            if self.seen_local.contains(&token)
                || self
                    .remote_attempts
                    .iter()
                    .any(|binding| binding.token == token)
            {
                bail!(
                    "remote attempt driver rejected reuse of task token {}",
                    token.0
                );
            }
            self.active_local
                .try_reserve(1)
                .context("remote attempt driver could not reserve active-local capacity")?;
            self.seen_local
                .try_reserve(1)
                .context("remote attempt driver could not reserve local-history capacity")?;
            self.pool.submit(submission)?;
            self.active_local.insert(token);
            self.seen_local.insert(token);
        }
        Ok(())
    }

    fn try_recv_event(&mut self) -> Result<Option<WorkerEvent>> {
        loop {
            let Some(event) = self.pool.try_recv_event()? else {
                return Ok(None);
            };
            if let Some(event) = self.accept_event(event)? {
                return Ok(Some(event));
            }
        }
    }

    fn recv_event(&mut self) -> Result<WorkerEvent> {
        loop {
            let event = self.pool.recv_event()?;
            if let Some(event) = self.accept_event(event)? {
                return Ok(event);
            }
            if self.pool.outstanding() == 0 {
                return Err(gate_error(
                    19,
                    "current attempt",
                    "all remaining remote completions were fenced or superseded",
                ));
            }
        }
    }
}

pub(crate) struct PreparedRemotePureAttemptV1 {
    attempt: AttemptIdV1,
    client: FabricAttemptClientV1,
    expected_server_principal_sha256: SemanticDigestV1,
    pinned_node: PinnedFabricNodeKeyV1,
    trusted_authorities: TrustedFabricAuthoritiesV1,
    target: FabricTargetBindingV1,
    submission: FabricSubmissionV1,
    capsule: ExecutionCapsuleV1,
    poll_interval: Duration,
    attempt_lifetime: Duration,
    coordinator_attempt_started: Instant,
    coordinator_attempt_deadline: Instant,
}

impl PreparedRemotePureAttemptV1 {
    pub(crate) fn attempt(&self) -> &AttemptIdV1 {
        &self.attempt
    }

    fn execute_remote(&self) -> Result<OValue> {
        let started = self.coordinator_attempt_started;
        let deadline = self.coordinator_attempt_deadline;
        let mut request = FabricRequestV1::SubmitPureAttempt(self.submission.clone());
        loop {
            if Instant::now() >= deadline {
                return Err(gate_error(
                    18,
                    "coordinator-observed deadline",
                    "remote attempt exhausted its monotonic budget before the next exchange",
                ));
            }
            let exchange = self
                .client
                .exchange(&request, deadline)
                .map_err(map_client_failure)?;
            require_server_principal(self, exchange.tls_server_principal_sha256())?;
            match exchange.response() {
                FabricResponseV1::TerminalCandidate(terminal) => {
                    return accept_terminal_candidate(
                        self,
                        exchange.coordinator_observed_unix_ms(),
                        started.elapsed(),
                        terminal,
                    )
                }
                FabricResponseV1::Accepted(status) | FabricResponseV1::Running(status) => {
                    validate_intermediate_status(self, status)?;
                    if Instant::now() >= deadline {
                        return Err(gate_error(
                            18,
                            "coordinator-observed deadline",
                            "remote attempt remained nonterminal past its monotonic coordinator budget",
                        ));
                    }
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    std::thread::sleep(self.poll_interval.min(remaining));
                    request = FabricRequestV1::QueryAttempt(FabricAttemptQueryV1::from_submission(
                        &self.submission,
                    ));
                }
                FabricResponseV1::Rejected(rejection) => {
                    validate_intermediate_status(self, rejection.status())?;
                    return Err(gate_error(
                        8,
                        "signed lease and issuer",
                        format!(
                            "provider rejected Fabric attempt [{}]: {}",
                            rejection.reason_code(),
                            rejection.message()
                        ),
                    ));
                }
                FabricResponseV1::Abandoned(abandonment) => {
                    validate_intermediate_status(self, abandonment.status())?;
                    return Err(gate_error(
                        19,
                        "current attempt",
                        format!(
                            "provider abandoned Fabric attempt [{}]: {}",
                            abandonment.reason_code(),
                            abandonment.message()
                        ),
                    ));
                }
            }
        }
    }
}

fn map_client_failure(error: FabricClientFailureV1) -> anyhow::Error {
    match error {
        FabricClientFailureV1::NodeAuthentication(error) => gate_error(
            2,
            "authenticated responding node",
            format!("Fabric TLS peer authentication failed: {error:#}"),
        ),
        FabricClientFailureV1::WrongServerPrincipal { expected, actual } => gate_error(
            2,
            "authenticated responding node",
            format!("Fabric TLS server principal `{actual}` differs from pinned `{expected}`"),
        ),
        FabricClientFailureV1::ResponseRepresentation(error) => gate_error(
            1,
            "canonical response representation",
            format!("Fabric response framing or canonical decoding failed: {error:#}"),
        ),
        FabricClientFailureV1::Deadline => gate_error(
            18,
            "coordinator-observed deadline",
            "Fabric transport exceeded the attempt's absolute monotonic deadline",
        ),
        FabricClientFailureV1::CoordinatorClock(error) => gate_error(
            18,
            "coordinator-observed deadline",
            format!("coordinator response clock failed: {error:#}"),
        ),
        FabricClientFailureV1::Connection(error) => {
            anyhow!("Fabric connection failed before candidate acceptance: {error:#}")
        }
        FabricClientFailureV1::RequestTransport(error) => {
            anyhow!("Fabric request transport failed before candidate acceptance: {error:#}")
        }
        FabricClientFailureV1::RequestPreparation(error) => {
            anyhow!("Fabric request preparation failed before candidate acceptance: {error:#}")
        }
        FabricClientFailureV1::ResponseTransport(error) => {
            anyhow!("Fabric response transport failed before candidate acceptance: {error:#}")
        }
    }
}

impl PreparedTask for PreparedRemotePureAttemptV1 {
    fn execute(self: Box<Self>, _context: &crate::executor::task::TaskContext) -> Result<OValue> {
        self.execute_remote()
            .map_err(crate::process::infrastructure_error)
    }
}

/// Derive and sign one remote attempt from the exact admitted operation and
/// its already materialized child values. Callers cannot supply source text,
/// renderer roles, semantic digests, or a logical task identity.
pub(crate) fn prepare_remote_pure_attempt_v1(
    config: &RemotePureExecutionConfigV1,
    admitted: &AdmittedExecution<'_>,
    frame: &GraphEvalFrame,
    flat: &[&OIr],
    plan: &ExecutionPlan,
    id: PlanNodeId,
) -> Result<PreparedRemotePureAttemptV1> {
    config.validate()?;
    let oir = *flat
        .get(id.0)
        .ok_or_else(|| anyhow!("remote renderer plan node {} is out of OIR bounds", id.0))?;
    let OIr::Exec {
        lang,
        env_id,
        attr,
        backend,
        body,
    } = oir
    else {
        bail!("remote pure execution accepts only an admitted Exec operation");
    };
    if lang != &backend.canonical
        || !EnvironmentRefV2::from_encoded(*env_id).is_fresh()
        || attr.is_some()
        || !backend.pure
        || backend.execution != ExecutionMode::InlineValue
    {
        bail!("admitted operation is outside the deterministic fresh trusted-inline profile");
    }
    let profile = trusted_inline_fabric_profile_v1(backend).map_err(anyhow::Error::new)?;
    if profile.realization_pipeline_sha256() != config.target.realization_pipeline_sha256() {
        bail!("explicit remote target does not bind this renderer realization pipeline");
    }

    let projection = build_admission_derived_projection(
        frame,
        flat,
        plan,
        id,
        body.len(),
        backend.canonical.as_str(),
        profile.renderer(),
        *profile.implementation_sha256(),
        admitted.admission().base_policy(),
    )?;
    let admission_sha256 = decode_sha256_hex(
        "execution admission",
        admitted.admission().admission_sha256(),
    )?;
    let logical_semantic = fresh_logical_task_sha256(
        &admission_sha256,
        projection.source_closure.closure_sha256(),
        projection.region.region_sha256(),
        projection.inputs.manifest_sha256(),
    )?;
    let logical_task = LogicalTaskIdV1::new(config.execution.clone(), logical_semantic)
        .map_err(anyhow::Error::new)?;
    // M3 has neither retry nor supersession. Every distinct logical task starts
    // at the protocol's first nonzero attempt generation.
    let attempt = AttemptIdV1::new(logical_task, 1).map_err(anyhow::Error::new)?;

    let lifetime_ms = duration_millis(config.attempt_lifetime, "attempt lifetime")?;
    let coordinator_attempt_started = Instant::now();
    let coordinator_attempt_deadline = coordinator_attempt_started
        .checked_add(config.attempt_lifetime)
        .context("remote attempt monotonic deadline overflowed")?;
    let issued_at = unix_millis_now()?;
    let expires_at = issued_at
        .checked_add(lifetime_ms)
        .context("remote attempt expiry overflowed Unix milliseconds")?;
    let output = OutputContractV1::for_renderer(
        REMOTE_RESULT_SLOT_V1,
        profile.renderer(),
        config.limits.max_output_bytes(),
    )
    .map_err(anyhow::Error::new)?;
    let capsule = ExecutionCapsuleV1::new(
        attempt.clone(),
        projection.region,
        admission_sha256,
        projection.inputs,
        output,
        expires_at.get(),
        config.limits.clone(),
    )
    .map_err(anyhow::Error::new)?;
    let capsule_bytes = encode_execution_capsule_v1(&capsule).map_err(anyhow::Error::new)?;
    let lease_nonce = fresh_lease_nonce()?;
    let lease = PlacementLeaseV3::new(
        config.authority_signer.key_id_digest(),
        lease_nonce,
        config.target.clone(),
        &projection.source_closure,
        &capsule,
        issued_at,
        expires_at,
    )
    .map_err(anyhow::Error::new)?;
    let signed_lease = config
        .authority_signer
        .sign_execution_lease(lease)
        .map_err(anyhow::Error::new)?;
    let submission =
        FabricSubmissionV1::new(signed_lease, projection.source_closure, capsule_bytes)
            .map_err(anyhow::Error::new)?;
    let mut trusted_authorities = TrustedFabricAuthoritiesV1::new();
    trusted_authorities.enroll(config.authority_signer.public_key());

    Ok(PreparedRemotePureAttemptV1 {
        attempt,
        client: config.client(),
        expected_server_principal_sha256: config.expected_server_principal_sha256.clone(),
        pinned_node: config.pinned_node.clone(),
        trusted_authorities,
        target: config.target.clone(),
        submission,
        capsule,
        poll_interval: config.poll_interval,
        attempt_lifetime: config.attempt_lifetime,
        coordinator_attempt_started,
        coordinator_attempt_deadline,
    })
}

struct AdmissionDerivedProjectionV1 {
    source_closure: FabricSourceClosureV1,
    region: SourceClosedRendererV1,
    inputs: InputManifestV1,
}

#[allow(clippy::too_many_arguments)]
fn build_admission_derived_projection(
    frame: &GraphEvalFrame,
    flat: &[&OIr],
    plan: &ExecutionPlan,
    id: PlanNodeId,
    admitted_body_len: usize,
    backend: &str,
    renderer: crate::execution_fabric::TrustedInlineRendererV1,
    implementation_sha256: Sha256DigestV1,
    base_policy: crate::execution_contract::Policy,
) -> Result<AdmissionDerivedProjectionV1> {
    let children = plan.child_schedule(id).map_err(anyhow::Error::msg)?;
    if children.len() != admitted_body_len {
        bail!("admitted renderer body and plan child projection differ");
    }
    let registered = BackendRegistry::global().registered_backend_tags();
    let closer = format!(")_{backend}");
    let mut source = format!("{backend}^(");
    let mut parts = Vec::with_capacity(children.len());
    let mut bindings = Vec::new();
    for child in children {
        let child_oir = *flat
            .get(child.0)
            .ok_or_else(|| anyhow!("renderer child {} is out of OIR bounds", child.0))?;
        match (&plan.nodes[child.0].kind, child_oir) {
            (PlanNodeKind::Text, OIr::Text(expected)) => {
                let OValue::Text { v } = frame.value(child)? else {
                    bail!(
                        "renderer literal child {} did not materialize OText",
                        child.0
                    );
                };
                if &v.utf8 != expected {
                    bail!("renderer literal child {} drifted after admission", child.0);
                }
                source.push_str(&escape_typed_body_literal(&v.utf8, &closer, &registered));
                parts.push(RendererPartV1::literal(v.utf8.clone()));
            }
            (PlanNodeKind::Store { .. }, _) | (_, OIr::Store { .. }) => {
                bail!("remote trusted-inline projection rejects Store children")
            }
            (_, OIr::Exec { .. }) => {
                let slot = format!("input_{:08}", bindings.len());
                let portable =
                    PortableOValue::try_from(frame.value(child)?).with_context(|| {
                        format!("renderer input child {} is not M2-portable", child.0)
                    })?;
                let record = PortableValueRecord::Core(portable);
                bindings
                    .push(InputBindingV1::new(slot.clone(), &record).map_err(anyhow::Error::new)?);
                source.push('$');
                source.push_str(&slot);
                parts.push(RendererPartV1::input(slot));
            }
            _ => bail!(
                "remote trusted-inline projection rejects Load, Invoke, and arbitrary child OIR"
            ),
        }
    }
    source.push_str(&closer);

    // Reparse and solve the exact fragment before signing it. The provider
    // independently repeats this reconstruction; these expected digests are
    // not a permission to skip that work.
    let mut parser = Parser::new(&source, &registered);
    let parsed = parser
        .parse_with_origins()
        .context("failed to parse admission-derived remote renderer source")?;
    let program = OIrProgram::lower(parsed.nodes());
    if program.nodes.len() != 1 {
        bail!("remote renderer source did not lower to one root operation");
    }
    let reconstructed_plan = program.plan();
    let mut graph = program
        .hgraph_for_plan(&reconstructed_plan)
        .map_err(anyhow::Error::msg)?;
    solve_types(&mut graph).context("failed to solve remote renderer source projection")?;
    let intent = ExecutionIntentV1::compile(
        source.as_bytes(),
        &program,
        &reconstructed_plan,
        &graph,
        base_policy,
    )
    .context("failed to compile remote renderer execution intent")?;
    let intent_sha256 = decode_sha256_hex("renderer intent", &intent.execution_intent_sha256)?;
    let oir_sha256 = decode_sha256_hex("renderer OIR", &intent.oir_sha256)?;
    let plan_sha256 = decode_sha256_hex("renderer plan", &intent.plan_sha256)?;
    let catalog_sha256 = decode_sha256_hex(
        "renderer catalog projection",
        &intent.backend_catalog_projection_sha256,
    )?;
    let source_closure = FabricSourceClosureV1::new(
        FABRIC_SOURCE_CLOSURE_DIALECT_V1,
        source,
        FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1,
        base_policy.name(),
        intent_sha256,
        oir_sha256,
        plan_sha256,
    )
    .map_err(anyhow::Error::new)?;
    let region = SourceClosedRendererV1::new(
        renderer,
        parts,
        oir_sha256,
        plan_sha256,
        catalog_sha256,
        implementation_sha256,
    )
    .map_err(anyhow::Error::new)?;
    let inputs = InputManifestV1::new(bindings).map_err(anyhow::Error::new)?;
    Ok(AdmissionDerivedProjectionV1 {
        source_closure,
        region,
        inputs,
    })
}

fn require_server_principal(
    expected: &PreparedRemotePureAttemptV1,
    actual_principal_sha256: &str,
) -> Result<()> {
    if actual_principal_sha256 != expected.expected_server_principal_sha256.as_sha256() {
        return Err(gate_error(
            2,
            "authenticated responding node",
            "Fabric TLS server principal differs from the explicitly selected node",
        ));
    }
    Ok(())
}

fn validate_intermediate_status(
    expected: &PreparedRemotePureAttemptV1,
    status: &FabricAttemptStatusV1,
) -> Result<()> {
    let lease = expected.submission.header().lease().lease();
    if status.attempt().task().execution() != expected.attempt.task().execution() {
        return Err(gate_error(
            4,
            "execution identity",
            "nonterminal Fabric status names a different global execution",
        ));
    }
    if status.attempt().task() != expected.attempt.task() {
        return Err(gate_error(
            5,
            "logical task identity",
            "nonterminal Fabric status names a different logical task",
        ));
    }
    if status.attempt().generation() != expected.attempt.generation() {
        return Err(gate_error(
            6,
            "current attempt generation",
            "nonterminal Fabric status names a stale or future attempt generation",
        ));
    }
    if status.node_id() != expected.target.node_id()
        || status.node_generation() != expected.target.node_generation()
        || status.execution_cell_incarnation() != expected.target.execution_cell_incarnation()
    {
        return Err(gate_error(
            7,
            "current node generation",
            "nonterminal Fabric status does not bind the retained node generation",
        ));
    }
    if status.issuer_key_id() != lease.issuer_key_id()
        || status.submission_binding_sha256()
            != expected.submission.header().submission_binding_sha256()
    {
        return Err(gate_error(
            8,
            "signed lease and issuer",
            "nonterminal Fabric status does not bind the retained issuer/submission",
        ));
    }
    if status.lease_nonce() != lease.lease_nonce() {
        return Err(gate_error(
            9,
            "lease nonce",
            "nonterminal Fabric status does not bind the retained one-use nonce",
        ));
    }
    Ok(())
}

fn accept_terminal_candidate(
    expected: &PreparedRemotePureAttemptV1,
    coordinator_observed_unix_ms: u64,
    coordinator_monotonic_elapsed: Duration,
    terminal: &FabricTerminalCandidateV1,
) -> Result<OValue> {
    // Gate 1 is normally established by the client codec plus authenticated
    // clean EOF. Recheck the exact terminal representation for direct callers
    // and tests without performing any later binding or content validation.
    let candidate = terminal
        .decoded_candidate_representation()
        .map_err(|error| gate_error(1, "canonical response representation", error))?;
    // Gate 2 was checked for this exact exchange before response dispatch.
    expected
        .pinned_node
        .authenticate_terminal_receipt(terminal.signed_receipt())
        .map_err(|error| gate_error(3, "trusted node key or channel binding", error))?;
    let receipt = terminal.signed_receipt().receipt();
    if receipt.attempt().task().execution() != expected.attempt.task().execution()
        || candidate.attempt().task().execution() != expected.attempt.task().execution()
    {
        return Err(gate_error(
            4,
            "execution identity",
            "terminal receipt names a different global execution",
        ));
    }
    if receipt.attempt().task() != expected.attempt.task()
        || candidate.attempt().task() != expected.attempt.task()
    {
        return Err(gate_error(
            5,
            "logical task identity",
            "terminal receipt names a different logical task",
        ));
    }
    if receipt.attempt().generation() != expected.attempt.generation()
        || candidate.attempt().generation() != expected.attempt.generation()
    {
        return Err(gate_error(
            6,
            "current attempt generation",
            "terminal receipt names a stale or future attempt generation",
        ));
    }
    if receipt.node_id() != expected.target.node_id()
        || receipt.node_generation() != expected.target.node_generation()
        || receipt.execution_cell_incarnation() != expected.target.execution_cell_incarnation()
    {
        return Err(gate_error(
            7,
            "current node generation",
            "terminal receipt names a different node generation or execution-cell incarnation",
        ));
    }

    let signed_lease = expected.submission.header().lease();
    expected
        .trusted_authorities
        .authenticate_execution_lease(signed_lease)
        .map_err(|error| gate_error(8, "signed lease and issuer", error))?;
    let lease = signed_lease.lease();
    let lease_sha256 = fabric_lease_sha256_v3(lease)
        .map_err(|error| gate_error(8, "signed lease and issuer", error))?;
    if receipt.issuer_key_id() != lease.issuer_key_id()
        || receipt.lease_sha256() != &lease_sha256
        || receipt.submission_binding_sha256()
            != expected.submission.header().submission_binding_sha256()
    {
        return Err(gate_error(
            8,
            "signed lease and issuer",
            "terminal receipt does not bind the retained authenticated lease/submission",
        ));
    }
    if receipt.lease_nonce() != lease.lease_nonce() {
        return Err(gate_error(
            9,
            "lease nonce",
            "terminal receipt lease nonce differs from the retained one-use authority",
        ));
    }

    let capsule_sha256 = expected
        .capsule
        .canonical_sha256()
        .map_err(|error| gate_error(10, "capsule digest", error))?;
    if receipt.capsule_sha256() != &capsule_sha256
        || receipt.capsule_sha256() != lease.capsule_sha256()
        || candidate.capsule_sha256() != &capsule_sha256
    {
        return Err(gate_error(
            10,
            "capsule digest",
            "terminal receipt capsule digest differs from the retained M2 capsule",
        ));
    }
    if receipt.source_closure_sha256()
        != expected
            .submission
            .header()
            .source_closure()
            .closure_sha256()
        || receipt.source_closure_sha256() != lease.source_closure_sha256()
    {
        return Err(gate_error(
            11,
            "source-closure digest",
            "terminal receipt source closure differs from the signed submission",
        ));
    }
    if receipt.input_manifest_sha256() != expected.capsule.inputs().manifest_sha256()
        || receipt.input_manifest_sha256() != lease.input_manifest_sha256()
        || candidate.input_manifest_sha256() != expected.capsule.inputs().manifest_sha256()
    {
        return Err(gate_error(
            12,
            "input-manifest digest",
            "terminal receipt input manifest differs from the frozen capsule",
        ));
    }
    if receipt.backend_catalog_sha256() != expected.capsule.region().backend_catalog_sha256()
        || receipt.backend_catalog_sha256() != lease.backend_catalog_sha256()
    {
        return Err(gate_error(
            13,
            "backend catalog digest",
            "terminal receipt backend catalog projection differs from the frozen region",
        ));
    }
    if receipt.backend_implementation_sha256()
        != expected.capsule.region().backend_implementation_sha256()
        || receipt.backend_implementation_sha256() != lease.backend_implementation_sha256()
        || candidate.region_sha256() != expected.capsule.region().region_sha256()
    {
        return Err(gate_error(
            14,
            "backend implementation digest",
            "terminal receipt backend implementation differs from the frozen region",
        ));
    }
    if receipt.output_contract_sha256() != expected.capsule.output().contract_sha256()
        || receipt.output_contract_sha256() != lease.output_contract_sha256()
        || candidate.output_contract_sha256() != expected.capsule.output().contract_sha256()
    {
        return Err(gate_error(
            15,
            "output-contract digest",
            "terminal receipt output contract differs from the frozen capsule",
        ));
    }

    let CandidateOutcomeV1::Succeeded { output } = candidate.outcome() else {
        return Err(gate_error(
            16,
            "output kind and fidelity",
            "trusted-inline provider returned a failed semantic candidate",
        ));
    };
    if output.slot() != expected.capsule.output().slot()
        || output.value().encoded().len() > expected.capsule.output().max_bytes() as usize
    {
        return Err(gate_error(
            15,
            "output-contract digest",
            "candidate output slot/size violates the frozen output contract",
        ));
    }
    if output.value_kind() != expected.capsule.output().value_kind()
        || output.fidelity() != expected.capsule.output().fidelity()
    {
        return Err(gate_error(
            16,
            "output kind and fidelity",
            "candidate output kind/fidelity differs from the frozen renderer contract",
        ));
    }

    receipt
        .candidate_payload()
        .validate_bytes(
            terminal.candidate_bytes(),
            crate::execution_fabric::MAX_EXECUTION_CANDIDATE_BYTES,
            "candidate",
        )
        .map_err(|error| gate_error(17, "exact output-content digest", error))?;
    if candidate.completed_unix_ms() != receipt.provider_completed_unix_ms() {
        return Err(gate_error(
            17,
            "exact output-content digest",
            "terminal receipt completion evidence differs from the exact candidate",
        ));
    }
    if output.value().content_sha256() != receipt.output_content_sha256() {
        return Err(gate_error(
            17,
            "exact output-content digest",
            "candidate portable output digest differs from the signed receipt",
        ));
    }
    let value = lower_exact_renderer_output(
        output.value_kind(),
        output
            .value()
            .decode()
            .map_err(|error| gate_error(17, "exact output-content digest", error))?,
    )?;

    if receipt.provider_completed_unix_ms() == 0
        || candidate.completed_unix_ms() > expected.capsule.deadline_unix_ms()
        || receipt.runtime_observation_ms() > lease.maximum_runtime_ms()
        || coordinator_monotonic_elapsed > expected.attempt_lifetime
    {
        return Err(gate_error(
            18,
            "coordinator-observed deadline",
            "terminal timing evidence exceeds or omits the signed/local monotonic budget",
        ));
    }
    if coordinator_observed_unix_ms == 0
        || coordinator_observed_unix_ms > expected.capsule.deadline_unix_ms()
    {
        return Err(gate_error(
            18,
            "coordinator-observed deadline",
            "coordinator observed the candidate outside its admitted absolute deadline",
        ));
    }
    Ok(value)
}

fn lower_exact_renderer_output(
    kind: OutputValueKindV1,
    record: PortableValueRecord,
) -> Result<OValue> {
    let PortableValueRecord::Core(PortableOValue::Text(text)) = record else {
        return Err(gate_error(
            17,
            "exact output-content digest",
            "Fabric V1 renderer output is not one canonical portable Text record",
        ));
    };
    Ok(match kind {
        OutputValueKindV1::Text => OValue::Text { v: text },
        OutputValueKindV1::Html => OValue::html(text.utf8),
    })
}

fn fresh_logical_task_sha256(
    admission_sha256: &Sha256DigestV1,
    source_closure_sha256: &Sha256DigestV1,
    region_sha256: &Sha256DigestV1,
    input_manifest_sha256: &Sha256DigestV1,
) -> Result<Sha256DigestV1> {
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce)
        .context("operating-system entropy failed for Fabric logical-task identity")?;
    let task_nonce = semantic_digest_bytes(REMOTE_LOGICAL_TASK_NONCE_DOMAIN_V1, &nonce);
    let mut material = Vec::with_capacity(32 * 5);
    material.extend_from_slice(admission_sha256);
    material.extend_from_slice(source_closure_sha256);
    material.extend_from_slice(region_sha256);
    material.extend_from_slice(input_manifest_sha256);
    material.extend_from_slice(&task_nonce);
    // The nonce distinguishes identical admitted occurrences without placing
    // a TaskToken, plan-node index, graph coordinate, or source-order ordinal
    // into the protocol identity. The resulting task coordinate remains fixed
    // for the lifetime of this prepared attempt and its exact resubmissions.
    Ok(semantic_digest_bytes(
        REMOTE_LOGICAL_TASK_DOMAIN_V1,
        &material,
    ))
}

fn fresh_lease_nonce() -> Result<SemanticDigestV1> {
    let mut entropy = [0_u8; 32];
    getrandom::fill(&mut entropy)
        .context("operating-system entropy failed for Fabric lease nonce")?;
    let digest = semantic_digest_bytes(REMOTE_LEASE_NONCE_DOMAIN_V1, &entropy);
    SemanticDigestV1::from_sha256(hex::encode(digest)).map_err(anyhow::Error::new)
}

fn semantic_digest_bytes(domain: &'static str, bytes: &[u8]) -> Sha256DigestV1 {
    let digest = SemanticDigestV1::hash_bytes(domain, bytes);
    decode_sha256_hex("semantic digest", digest.as_sha256())
        .expect("SemanticDigestV1 always contains one lowercase SHA-256")
}

fn decode_sha256_hex(label: &str, value: &str) -> Result<Sha256DigestV1> {
    let decoded = hex::decode(value).with_context(|| format!("{label} is not hexadecimal"))?;
    decoded
        .try_into()
        .map_err(|_| anyhow!("{label} is not exactly 32 bytes"))
}

fn unix_millis_now() -> Result<UnixMillisV1> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("coordinator wall clock precedes the Unix epoch")?;
    Ok(UnixMillisV1::new(
        u64::try_from(elapsed.as_millis()).context("Unix millisecond timestamp exceeds u64")?,
    ))
}

fn duration_millis(duration: Duration, label: &str) -> Result<u64> {
    u64::try_from(duration.as_millis()).with_context(|| format!("{label} exceeds u64 milliseconds"))
}

fn physical_attempt_coordinate_v1(attempt: &AttemptIdV1) -> Result<PhysicalAttemptCoordinateV1> {
    PhysicalAttemptCoordinateV1::new(
        *attempt.task().execution().as_bytes(),
        *attempt.task().semantic_sha256(),
        attempt.generation(),
    )
}

fn gate_error(number: u8, name: &'static str, detail: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("Fabric acceptance gate {number:02} ({name}) failed: {detail}")
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;
