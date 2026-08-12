use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::effects::{EffectConfidence, EffectSummary, Fallibility, ResourceKey};
use crate::executor::parallel;
use crate::hgraph::{
    HEdgeKind, HGraph, HNodeKind, MemOrder, OcoreOpKind, OpKind, PortRole, ValueState,
};
use crate::ir::{
    BackendAdapterKind, BackendRegistry, ExecutionPlan, InvokeMode, OIrProgram, PlanNodeKind,
    BACKEND_CATALOG_SCHEMA_V1,
};
use crate::value::GroupMode;

use super::fact::{
    BackendArtifactStateV1, BackendArtifactV1, CapabilityDispositionV1, CostEstimateV1,
    DispatchAdapterV1, DispatchContractV1, DispatchLaneV1, DispatchSemanticsV1, EffectContractV1,
    EvidenceBindingsV2, EvidenceBundleV4, EvidenceProvenance, FailureClassV1, FailureContractV1,
    NodeEvidence, PlacementContractV1, ResourceDemandContractV1, RuntimeBindingV1,
    RuntimeSnapshotKindV1, TypeContractV1, ANALYZER_ID_V4, EVIDENCE_SCHEMA_V4,
};

/// Capture the current executable plus each separate compatibility-shim
/// artifact actually selected by the plan's canonical adapter kinds. Missing
/// legacy shims are represented explicitly instead of changing the historical
/// error timing during conservative coordinator work.
pub fn runtime_binding_from_directory(
    plan: &ExecutionPlan,
    shim_dir: &Path,
    context: &[(&str, &str)],
) -> RuntimeBindingV1 {
    let backends = legacy_python_shim_backends(plan);
    let artifacts = backends
        .into_iter()
        .map(|backend| {
            let path = BackendRegistry::global().resolve_shim_path(shim_dir, &backend);
            BackendArtifactV1 {
                canonical_backend: backend,
                resolved_identity: path_identity(&path),
                state: backend_artifact_state(&path),
            }
        })
        .collect::<Vec<_>>();
    build_runtime_binding(
        RuntimeSnapshotKindV1::Execution,
        artifacts,
        backend_catalog_projection_sha256(plan),
        context,
    )
}

/// Capture the non-executing `olangc --target ir` adapter snapshot. The input
/// is the same merged bundled/override inventory that a generated runtime
/// would embed; no adapter is launched.
pub fn runtime_binding_from_adapter_bytes(
    plan: &ExecutionPlan,
    adapters: &[(String, Vec<u8>)],
    context: &[(&str, &str)],
) -> RuntimeBindingV1 {
    let by_name = adapters.iter().cloned().collect::<BTreeMap<_, _>>();
    let artifacts = legacy_python_shim_backends(plan)
        .into_iter()
        .map(|backend| {
            let candidates = [
                format!("{backend}_shim.py"),
                format!("{backend}_shim"),
                format!("{backend}.py"),
                backend.clone(),
            ];
            let selected = candidates
                .iter()
                .find(|candidate| by_name.contains_key(*candidate))
                .cloned()
                .unwrap_or_else(|| candidates[0].clone());
            BackendArtifactV1 {
                canonical_backend: backend,
                resolved_identity: format!("adapter:{selected}"),
                state: by_name
                    .get(&selected)
                    .map(|bytes| BackendArtifactStateV1::Hashed {
                        sha256: sha256_bytes(bytes),
                    })
                    .unwrap_or(BackendArtifactStateV1::Missing),
            }
        })
        .collect::<Vec<_>>();
    build_runtime_binding(
        RuntimeSnapshotKindV1::Inspection,
        artifacts,
        backend_catalog_projection_sha256(plan),
        context,
    )
}

/// Bind only the canonical backend specifications actually referenced by the
/// plan. This projection is deliberately capacity-neutral: it performs no
/// executable lookup, PATH scan, invocation, health probe, or authorization
/// check. Unknown extension backends remain explicit instead of being
/// confused with a registered builtin.
pub(crate) fn backend_catalog_projection_sha256(plan: &ExecutionPlan) -> String {
    let backends = plan
        .nodes
        .iter()
        .filter_map(|node| match &node.kind {
            PlanNodeKind::Exec { backend, .. } => Some(backend.canonical.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let registry = BackendRegistry::global();
    let mut hash = CanonicalHasher::new("ostadix-backend-catalog-projection/v1");
    hash.field(BACKEND_CATALOG_SCHEMA_V1.as_bytes());
    for backend in backends {
        hash.field(backend.as_bytes());
        match registry.specification_sha256(backend) {
            Some(specification_sha256) => {
                hash.tag("registered");
                hash.field(specification_sha256.as_bytes());
            }
            None => hash.tag("unregistered"),
        }
    }
    hash.finish()
}

/// Validate and rebuild the exact solved static graph used by both stable
/// execution-intent projection and live evidence admission. Keeping this
/// routine shared prevents a descriptive intent from blessing a graph that
/// the admission analyzer would later reject as noncanonical.
pub(crate) fn validate_canonical_solved_graph(
    program: &OIrProgram,
    plan: &ExecutionPlan,
    graph: &HGraph,
) -> Result<()> {
    if plan != &program.plan() {
        anyhow::bail!(
            "analysis requires the canonical ExecutionPlan derived from the exact lowered OIR"
        );
    }
    let flat = program.flatten_for_plan();
    crate::eval::validate_execution_metadata(&flat)
        .context("analysis rejected invalid OIR execution metadata")?;
    graph
        .validate_execution_source(program, plan)
        .map_err(anyhow::Error::msg)
        .context("analysis rejected OIR/plan/HGraph provenance")?;
    let mut canonical = program
        .hgraph_for_plan(plan)
        .map_err(anyhow::Error::msg)
        .context("analysis could not rebuild the canonical HGraph")?;
    crate::hgraph::solve::solve_types(&mut canonical)
        .context("analysis could not solve the canonical HGraph")?;
    if graph_sha256(&canonical) != graph_sha256(graph) {
        anyhow::bail!(
            "analysis requires the exact canonical solved HGraph; caller graph is unsolved or structurally divergent"
        );
    }
    Ok(())
}

/// Return only backends whose canonical adapter actually consumes a Python
/// shim artifact. `ExecutionMode::Shim` is intentionally not enough here:
/// native Rust backends also use framed hosted execution, but their adapter is
/// part of the already-bound current Ostadix executable.
fn legacy_python_shim_backends(plan: &ExecutionPlan) -> BTreeSet<String> {
    let registry = BackendRegistry::global();
    plan.nodes
        .iter()
        .filter_map(|node| match &node.kind {
            PlanNodeKind::Exec { backend, .. }
                if registry.adapter_for(&backend.canonical)
                    == BackendAdapterKind::LegacyPythonShim =>
            {
                Some(backend.canonical.clone())
            }
            _ => None,
        })
        .collect()
}

fn build_runtime_binding(
    snapshot_kind: RuntimeSnapshotKindV1,
    mut backend_artifacts: Vec<BackendArtifactV1>,
    backend_catalog_projection_sha256: String,
    context: &[(&str, &str)],
) -> RuntimeBindingV1 {
    backend_artifacts.push(current_executable_artifact());
    backend_artifacts.sort_by(|left, right| {
        (&left.canonical_backend, &left.resolved_identity)
            .cmp(&(&right.canonical_backend, &right.resolved_identity))
    });

    let mut backend_hash = CanonicalHasher::new("ostadix-backend-set/v1");
    for artifact in &backend_artifacts {
        backend_hash.field(artifact.canonical_backend.as_bytes());
        backend_hash.field(artifact.resolved_identity.as_bytes());
        encode_backend_artifact_state(&mut backend_hash, &artifact.state);
    }
    let backend_set_sha256 = backend_hash.finish();

    let mut environment = CanonicalHasher::new("ostadix-execution-environment/v1");
    environment.field(snapshot_kind.name().as_bytes());
    environment.field(backend_set_sha256.as_bytes());
    if let Ok(current_dir) = std::env::current_dir() {
        environment.field(&os_bytes(current_dir.as_os_str()));
    } else {
        environment.field(b"current-dir-unavailable");
    }
    let mut vars = std::env::vars_os()
        .map(|(key, value)| (os_bytes(&key).into_owned(), os_bytes(&value).into_owned()))
        .collect::<Vec<_>>();
    vars.sort();
    for (key, value) in vars {
        environment.field(&key);
        environment.field(&value);
    }
    let mut context = context.to_vec();
    context.sort_unstable();
    for (key, value) in context {
        environment.field(key.as_bytes());
        environment.field(value.as_bytes());
    }
    let environment_sha256 = environment.finish();

    let mut ambient_world = CanonicalHasher::new("ostadix-ambient-hostworld-snapshot/v1");
    ambient_world.field(environment_sha256.as_bytes());
    // WASI preview1 has no process ID concept; `std::process::id()` panics
    // there ("no pids on this platform"). Substitute a fixed placeholder so
    // the ambient-world fingerprint stays well-defined on wasm targets.
    #[cfg(not(target_family = "wasm"))]
    let pid = std::process::id();
    #[cfg(target_family = "wasm")]
    let pid: u32 = 0;
    ambient_world.field(&pid.to_be_bytes());
    let ambient_world_sha256 = ambient_world.finish();

    RuntimeBindingV1 {
        snapshot_kind,
        backend_artifacts,
        backend_catalog_projection_sha256,
        backend_set_sha256,
        environment_sha256,
        ambient_world_sha256,
    }
}

fn current_executable_artifact() -> BackendArtifactV1 {
    match std::env::current_exe() {
        Ok(path) => BackendArtifactV1 {
            canonical_backend: "__ostadix_current_executable__".to_string(),
            resolved_identity: path_identity(&path),
            state: backend_artifact_state(&path),
        },
        Err(error) => BackendArtifactV1 {
            canonical_backend: "__ostadix_current_executable__".to_string(),
            resolved_identity: "current-executable:unavailable".to_string(),
            state: BackendArtifactStateV1::Unreadable {
                error_kind: format!("CurrentExe::{:?}", error.kind()),
            },
        },
    }
}

fn backend_artifact_state(path: &Path) -> BackendArtifactStateV1 {
    match fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => BackendArtifactStateV1::NonRegular,
        Ok(_) => match fs::read(path) {
            Ok(bytes) => BackendArtifactStateV1::Hashed {
                sha256: sha256_bytes(&bytes),
            },
            Err(error) => BackendArtifactStateV1::Unreadable {
                error_kind: format!("{:?}", error.kind()),
            },
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            BackendArtifactStateV1::Missing
        }
        Err(error) => BackendArtifactStateV1::Unreadable {
            error_kind: format!("{:?}", error.kind()),
        },
    }
}

fn path_identity(path: &Path) -> String {
    format!("path-bytes:{}", hex::encode(os_bytes(path.as_os_str())))
}

fn encode_backend_artifact_state(hash: &mut CanonicalHasher, state: &BackendArtifactStateV1) {
    hash.tag(state.name());
    match state {
        BackendArtifactStateV1::Hashed { sha256 } => hash.field(sha256.as_bytes()),
        BackendArtifactStateV1::Unreadable { error_kind } => {
            hash.field(error_kind.as_bytes());
        }
        BackendArtifactStateV1::Missing | BackendArtifactStateV1::NonRegular => {}
    }
}

/// Produce hard evidence and explicitly unknown soft estimates for one solved
/// executable HGraph. This does not authorize execution; `admit_execution`
/// rechecks every digest and compiles the evidence into readiness inputs.
pub fn analyze_execution(
    program: &OIrProgram,
    plan: &ExecutionPlan,
    graph: &HGraph,
    runtime: RuntimeBindingV1,
) -> Result<EvidenceBundleV4> {
    if runtime.backend_catalog_projection_sha256 != backend_catalog_projection_sha256(plan) {
        anyhow::bail!(
            "runtime evidence backend catalog projection is stale or belongs to another ExecutionPlan"
        );
    }
    let current_executable = runtime
        .backend_artifacts
        .iter()
        .filter(|artifact| artifact.canonical_backend == "__ostadix_current_executable__")
        .collect::<Vec<_>>();
    if current_executable.len() != 1 {
        anyhow::bail!("runtime evidence requires exactly one reserved current-executable artifact");
    }
    // WASI preview1 sandboxes have no filesystem access to their own module
    // bytes (no preopen to self), so `current_executable_artifact()` can
    // never produce a `Hashed` state there. The self-hash provenance
    // guarantee is unsatisfiable by sandbox design on wasm, not a bug in a
    // given program, so this admission check is relaxed only for wasm
    // targets; non-wasm targets keep the full guarantee unchanged.
    #[cfg(not(target_family = "wasm"))]
    if runtime.snapshot_kind == RuntimeSnapshotKindV1::Execution
        && !matches!(
            current_executable[0].state,
            BackendArtifactStateV1::Hashed { .. }
        )
    {
        anyhow::bail!("execution evidence requires a readable, hash-bound current executable");
    }
    validate_canonical_solved_graph(program, plan, graph)
        .context("evidence analyzer rejected noncanonical static execution input")?;
    let flat = program.flatten_for_plan();

    let bindings = evidence_bindings(program, plan, graph, &runtime);
    let mut nodes = Vec::with_capacity(graph.op_map.len());
    for info in graph.exec_ops_ordered() {
        let output = graph.node(info.value_output).with_context(|| {
            format!(
                "operation {} has no distinguished value output",
                info.plan_node.0
            )
        })?;
        let summary = graph
            .effect_summary(info.plan_node)
            .with_context(|| format!("operation {} has no effect summary", info.plan_node.0))?;
        let effect_provenance = effect_provenance(summary.confidence);
        let (reads, writes) = summary.scheduling_accesses();
        let worker_kind = parallel::classify(plan, flat[info.plan_node.0], info.plan_node)
            .filter(|_| parallel::effect_contract_worker_safe(summary, flat[info.plan_node.0]));
        let worker_candidate = worker_kind.is_some();
        let dispatch_adapter = worker_kind
            .as_ref()
            .map(parallel::TaskKind::adapter)
            .unwrap_or(DispatchAdapterV1::CoordinatorV1);
        let (capability_disposition, capability_provenance) =
            capability_contract(&plan.nodes[info.plan_node.0].kind);

        nodes.push(NodeEvidence {
            plan_node: info.plan_node,
            type_contract: TypeContractV1 {
                constraints_solved: true,
                output_domain_bits: output.domain.bits(),
                output_representation_bits: output.rep.bits(),
                output_fidelity: output
                    .fidelity
                    .as_ref()
                    .map(|fidelity| {
                        serde_json::to_string(fidelity)
                            .expect("serializing fidelity into memory cannot fail")
                    })
                    .unwrap_or_else(|| "unknown".to_string()),
                provenance: EvidenceProvenance::CompilerVerified,
            },
            effect_contract: EffectContractV1 {
                reads: reads.into_iter().collect(),
                writes: writes.into_iter().collect(),
                footprint_closed: !summary.unknown && effect_provenance.may_close_unknown_effect(),
                provenance: effect_provenance,
            },
            dispatch_contract: DispatchContractV1 {
                lane: if worker_candidate {
                    DispatchLaneV1::LocalWorker
                } else {
                    DispatchLaneV1::Coordinator
                },
                adapter: dispatch_adapter,
                semantics: if dispatch_adapter == DispatchAdapterV1::AutonomousEphemeralShimV1 {
                    DispatchSemanticsV1::ExplicitAutonomousUnordered
                } else {
                    DispatchSemanticsV1::StrictEquivalent
                },
                send_only_preparation: worker_candidate,
                provenance: if worker_candidate {
                    EvidenceProvenance::TrustedAdapter
                } else {
                    EvidenceProvenance::CompilerVerified
                },
            },
            capability_disposition,
            capability_provenance,
            placement: if worker_candidate {
                PlacementContractV1::LocalWorker
            } else {
                PlacementContractV1::LocalCoordinator
            },
            placement_provenance: EvidenceProvenance::CompilerVerified,
            failure_contract: FailureContractV1 {
                class: failure_class(summary, dispatch_adapter),
                cancellation_safe: summary.is_verified_pure_infallible(),
                provenance: effect_provenance,
            },
            // V4 preserves a bounded, evidence-bound adapter set. Unknown
            // ceilings remain explicit and cannot remove topology edges.
            resource_demand: ResourceDemandContractV1 {
                cpu_units: Some(1),
                hard_memory_bytes: None,
                file_descriptors: (dispatch_adapter
                    == DispatchAdapterV1::AutonomousEphemeralShimV1)
                    .then_some(3),
                process_slots: (dispatch_adapter == DispatchAdapterV1::AutonomousEphemeralShimV1)
                    .then_some(1),
                provenance: EvidenceProvenance::Unknown,
            },
            cost_estimate: CostEstimateV1::unknown(),
        });
    }

    Ok(EvidenceBundleV4 {
        schema: EVIDENCE_SCHEMA_V4,
        analyzer: ANALYZER_ID_V4,
        bindings,
        runtime,
        nodes,
    })
}

pub(crate) fn evidence_bindings(
    program: &OIrProgram,
    plan: &ExecutionPlan,
    graph: &HGraph,
    runtime: &RuntimeBindingV1,
) -> EvidenceBindingsV2 {
    EvidenceBindingsV2 {
        oir_sha256: oir_sha256(program),
        plan_sha256: sha256_bytes(plan.to_text().as_bytes()),
        analyzed_graph_sha256: graph_sha256(graph),
        backend_catalog_projection_sha256: runtime.backend_catalog_projection_sha256.clone(),
        backend_set_sha256: runtime.backend_set_sha256.clone(),
        environment_sha256: runtime.environment_sha256.clone(),
        ambient_world_sha256: runtime.ambient_world_sha256.clone(),
        analyzer_sha256: sha256_bytes(ANALYZER_ID_V4.as_bytes()),
    }
}

pub(crate) fn graph_sha256(graph: &HGraph) -> String {
    let mut hash = CanonicalHasher::new("ostadix-solved-executable-hgraph/v1");
    for id in graph.node_ids() {
        let node = &graph.nodes[&id];
        hash.u64(id.0);
        match &node.kind {
            HNodeKind::Value => hash.tag("value"),
            HNodeKind::ResourceState { resource, version } => {
                hash.tag("resource-state");
                encode_resource_key(&mut hash, resource);
                hash.u64(*version);
            }
            HNodeKind::Completion { plan_node } => {
                hash.tag("completion");
                hash.usize(plan_node.0);
            }
            HNodeKind::BranchControl { label, version } => {
                hash.tag("branch-control");
                hash.field(label.as_bytes());
                hash.u64(*version);
            }
            HNodeKind::AdmissionEvidence { plan_node, fact } => {
                hash.tag("admission-evidence");
                hash.usize(plan_node.0);
                hash.field(fact.name().as_bytes());
            }
        }
        hash.u16(node.domain.bits());
        hash.u16(node.rep.bits());
        match &node.value {
            Some(value) => {
                hash.tag("materialized-value");
                hash.field(value.type_name().as_bytes());
                hash.field(value.content_identity().as_bytes());
            }
            None => hash.tag("no-value"),
        }
        match node.actor {
            Some(actor) => {
                hash.tag("actor");
                hash.u32(actor.lang);
                hash.u32(actor.env);
            }
            None => hash.tag("no-actor"),
        }
        match &node.fidelity {
            Some(fidelity) => hash.field(
                &serde_json::to_vec(fidelity)
                    .expect("serializing fidelity into memory cannot fail"),
            ),
            None => hash.tag("no-fidelity"),
        }
        encode_value_state(&mut hash, &node.state);
        hash.optional_u64(node.producer.map(|edge| edge.0));
        let mut consumers = node.consumers.clone();
        consumers.sort();
        for consumer in consumers {
            hash.u64(consumer.0);
        }
        hash.tag("end-consumers");
        hash.optional_usize(node.plan_node.map(|plan_node| plan_node.0));
    }

    for edge_id in graph.edge_ids() {
        let edge = &graph.edges[&edge_id];
        hash.tag("constraint-edge");
        hash.u64(edge_id.0);
        encode_op_kind(&mut hash, &edge.kind);
        for port in &edge.ports {
            encode_port(&mut hash, port.role, port.node.0);
        }
        hash.tag("end-ports");
    }

    for info in graph.exec_ops_ordered() {
        hash.tag("execute-edge");
        hash.usize(info.plan_node.0);
        hash.u64(info.edge.0);
        hash.u64(info.ordinal);
        hash.u64(info.value_output.0);
        let edge = &graph.exec_edges[&info.edge];
        if let HEdgeKind::Execute(op) = &edge.op {
            encode_executable_op(&mut hash, op);
        } else {
            hash.tag("invalid-execute-edge");
        }
        for input in &info.inputs {
            hash.u64(input.0);
        }
        hash.tag("end-inputs");
        for output in &info.outputs {
            hash.u64(output.0);
        }
        hash.tag("end-outputs");
    }

    let mut effects = graph.effect_summaries.iter().collect::<Vec<_>>();
    effects.sort_by_key(|(plan_node, _)| plan_node.0);
    for (plan_node, summary) in effects {
        hash.tag("effect-summary");
        hash.usize(plan_node.0);
        encode_effect_summary(&mut hash, summary);
    }
    let mut sequences = graph.sequence_dependencies.clone();
    sequences.sort_by_key(|dependency| {
        (
            dependency.predecessor.0,
            dependency.successor.0,
            dependency.completion.0,
        )
    });
    for dependency in sequences {
        hash.tag("sequence-dependency");
        hash.usize(dependency.predecessor.0);
        hash.usize(dependency.successor.0);
        hash.u64(dependency.completion.0);
    }
    for root in &graph.root_nodes {
        hash.tag("root");
        hash.u64(root.0);
    }
    let mut bindings = graph.bindings.iter().collect::<Vec<_>>();
    bindings.sort_by_key(|(name, _)| *name);
    for (name, node) in bindings {
        hash.tag("binding");
        hash.field(name.as_bytes());
        hash.u64(node.0);
    }
    hash.finish()
}

pub(crate) fn evidence_bundle_sha256(bundle: &EvidenceBundleV4) -> String {
    let mut hash = CanonicalHasher::new("ostadix-evidence-bundle/v4");
    hash.field(bundle.schema.as_bytes());
    hash.field(bundle.analyzer.as_bytes());
    for binding in [
        &bundle.bindings.oir_sha256,
        &bundle.bindings.plan_sha256,
        &bundle.bindings.analyzed_graph_sha256,
        &bundle.bindings.backend_catalog_projection_sha256,
        &bundle.bindings.backend_set_sha256,
        &bundle.bindings.environment_sha256,
        &bundle.bindings.ambient_world_sha256,
        &bundle.bindings.analyzer_sha256,
    ] {
        hash.field(binding.as_bytes());
    }
    for node in &bundle.nodes {
        hash.usize(node.plan_node.0);
        hash.bool(node.type_contract.constraints_solved);
        hash.u16(node.type_contract.output_domain_bits);
        hash.u16(node.type_contract.output_representation_bits);
        hash.field(node.type_contract.output_fidelity.as_bytes());
        hash.field(node.type_contract.provenance.name().as_bytes());
        for resource in &node.effect_contract.reads {
            hash.tag("read");
            encode_resource_key(&mut hash, resource);
        }
        for resource in &node.effect_contract.writes {
            hash.tag("write");
            encode_resource_key(&mut hash, resource);
        }
        hash.bool(node.effect_contract.footprint_closed);
        hash.field(node.effect_contract.provenance.name().as_bytes());
        hash.field(node.dispatch_contract.lane.name().as_bytes());
        hash.field(node.dispatch_contract.adapter.name().as_bytes());
        hash.field(node.dispatch_contract.semantics.name().as_bytes());
        hash.bool(node.dispatch_contract.send_only_preparation);
        hash.field(node.dispatch_contract.provenance.name().as_bytes());
        hash.field(node.capability_disposition.name().as_bytes());
        hash.field(node.capability_provenance.name().as_bytes());
        hash.field(node.placement.name().as_bytes());
        hash.field(node.placement_provenance.name().as_bytes());
        hash.field(node.failure_contract.class.name().as_bytes());
        hash.bool(node.failure_contract.cancellation_safe);
        hash.field(node.failure_contract.provenance.name().as_bytes());
        hash.optional_u64(node.resource_demand.cpu_units.map(u64::from));
        hash.optional_u64(node.resource_demand.hard_memory_bytes);
        hash.optional_u64(node.resource_demand.file_descriptors.map(u64::from));
        hash.optional_u64(node.resource_demand.process_slots.map(u64::from));
        hash.field(node.resource_demand.provenance.name().as_bytes());
        hash.optional_u64(node.cost_estimate.expected_duration_micros);
        hash.optional_u64(
            node.cost_estimate
                .confidence_parts_per_million
                .map(u64::from),
        );
        hash.field(node.cost_estimate.provenance.name().as_bytes());
    }
    hash.finish()
}

pub(crate) fn digest_fields(domain: &str, fields: &[&str]) -> String {
    let mut hash = CanonicalHasher::new(domain);
    for field in fields {
        hash.field(field.as_bytes());
    }
    hash.finish()
}

pub(crate) fn oir_sha256(program: &OIrProgram) -> String {
    let mut hash = CanonicalHasher::new("ostadix-lowered-oir-source/v1");
    // The OIR text contains every lowered body byte and a canonical plan dump.
    // The separate plan digest prevents this lowered-source identity from
    // being mistaken for an original-file byte digest.
    hash.field(program.to_text().as_bytes());
    hash.finish()
}

fn effect_provenance(confidence: EffectConfidence) -> EvidenceProvenance {
    match confidence {
        EffectConfidence::Verified => EvidenceProvenance::CompilerVerified,
        EffectConfidence::Conservative => EvidenceProvenance::Unknown,
        EffectConfidence::UserDeclared => EvidenceProvenance::UserDeclared,
    }
}

fn failure_class(summary: &EffectSummary, dispatch_adapter: DispatchAdapterV1) -> FailureClassV1 {
    match summary.fallibility {
        Fallibility::Infallible => FailureClassV1::Infallible,
        Fallibility::MayFail
            if dispatch_adapter == DispatchAdapterV1::AutonomousEphemeralShimV1
                && summary.unknown
                && summary.actor_state.is_none() =>
        {
            FailureClassV1::MayFailUnorderedExternalEffects
        }
        Fallibility::MayFail
            if summary.writes.is_empty()
                && !summary.unknown
                && !summary.network
                && !summary.spawn
                && !summary.clock
                && summary.actor_state.is_none()
                && summary.reads.iter().all(|resource| {
                    matches!(resource, crate::effects::ResourceKey::ScopeBinding(_))
                }) =>
        {
            FailureClassV1::MayFailNoExternalEffects
        }
        Fallibility::MayFail => FailureClassV1::Unknown,
    }
}

fn capability_contract(kind: &PlanNodeKind) -> (CapabilityDispositionV1, EvidenceProvenance) {
    match kind {
        PlanNodeKind::Exec { .. } => (
            CapabilityDispositionV1::DeferredRuntimeCheck,
            EvidenceProvenance::CompilerVerified,
        ),
        PlanNodeKind::Call { .. }
        | PlanNodeKind::Request { .. }
        | PlanNodeKind::Schedule { .. } => (
            CapabilityDispositionV1::DeferredRuntimeCheck,
            EvidenceProvenance::CompilerVerified,
        ),
        _ => (
            CapabilityDispositionV1::NotRequired,
            EvidenceProvenance::CompilerVerified,
        ),
    }
}

fn encode_effect_summary(hash: &mut CanonicalHasher, summary: &EffectSummary) {
    hash.bool(summary.deterministic);
    hash.tag(match summary.fallibility {
        Fallibility::Infallible => "infallible",
        Fallibility::MayFail => "may-fail",
    });
    for resource in &summary.reads {
        hash.tag("read");
        encode_resource_key(hash, resource);
    }
    for resource in &summary.writes {
        hash.tag("write");
        encode_resource_key(hash, resource);
    }
    match &summary.actor_state {
        Some(actor) => {
            hash.tag("actor-state");
            hash.field(actor.canonical_language.as_bytes());
            hash.u32(actor.environment);
        }
        None => hash.tag("no-actor-state"),
    }
    hash.bool(summary.unknown);
    hash.bool(summary.network);
    hash.bool(summary.spawn);
    hash.bool(summary.clock);
    hash.tag(match summary.confidence {
        EffectConfidence::Verified => "verified",
        EffectConfidence::Conservative => "conservative",
        EffectConfidence::UserDeclared => "user-declared",
    });
}

/// Encode the semantic resource variant, not its human-facing `Display`
/// spelling. Several distinct variants intentionally render through the same
/// conservative umbrella (for example `Network("*")` and
/// `NetworkUnknown`), so Display cannot bind an exact admitted graph.
fn encode_resource_key(hash: &mut CanonicalHasher, resource: &ResourceKey) {
    match resource {
        ResourceKey::HostWorld => hash.tag("host-world"),
        ResourceKey::WorldState(identity) => {
            hash.tag("world-state");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::GovernorState(identity) => {
            hash.tag("governor-state");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::NodeState(identity) => {
            hash.tag("node-state");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::DomainState(identity) => {
            hash.tag("domain-state");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::ProcessState(identity) => {
            hash.tag("process-state");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::GovernedResource(identity) => {
            hash.tag("governed-resource");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::ObjectState(identity) => {
            hash.tag("object-state");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::CapabilityState(identity) => {
            hash.tag("capability-state");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::NamespaceState(identity) => {
            hash.tag("namespace-state");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::TaskState(identity) => {
            hash.tag("task-state");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::ArtifactState(identity) => {
            hash.tag("artifact-state");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::DeviceState(identity) => {
            hash.tag("device-state");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::AcceleratorState(identity) => {
            hash.tag("accelerator-state");
            encode_serialized_identity(hash, identity);
        }
        ResourceKey::EvaluatorState => hash.tag("evaluator-state"),
        ResourceKey::ScopeBinding(name) => {
            hash.tag("scope-binding");
            hash.field(name.as_bytes());
        }
        ResourceKey::ProjectPath(path) => {
            hash.tag("project-path");
            hash.field(path.as_bytes());
        }
        ResourceKey::HostPath(path) => {
            hash.tag("host-path");
            hash.field(path.as_bytes());
        }
        ResourceKey::EnvVar(name) => {
            hash.tag("environment-variable");
            hash.field(name.as_bytes());
        }
        ResourceKey::Stdio => hash.tag("stdio"),
        ResourceKey::Network(endpoint) => {
            hash.tag("network-endpoint");
            hash.field(endpoint.as_bytes());
        }
        ResourceKey::NetworkUnknown => hash.tag("network-unknown"),
        ResourceKey::Service(service) => {
            hash.tag("service");
            hash.field(service.as_bytes());
        }
        ResourceKey::ActorState(actor) => {
            hash.tag("actor-state");
            hash.field(actor.canonical_language.as_bytes());
            hash.u32(actor.environment);
        }
    }
}

fn encode_serialized_identity<T: serde::Serialize>(hash: &mut CanonicalHasher, identity: &T) {
    let bytes = serde_json::to_vec(identity)
        .expect("validated World identities must serialize into canonical evidence");
    hash.field(&bytes);
}

fn encode_value_state(hash: &mut CanonicalHasher, state: &ValueState) {
    match state {
        ValueState::Unresolved => hash.tag("unresolved"),
        ValueState::Materialized => hash.tag("materialized"),
        ValueState::Failed(message) => {
            hash.tag("failed");
            hash.field(message.as_bytes());
        }
        ValueState::DisabledByBranch => hash.tag("disabled-by-branch"),
    }
}

fn encode_port(hash: &mut CanonicalHasher, role: PortRole, node: u64) {
    hash.tag(match role {
        PortRole::Input => "input",
        PortRole::Output => "output",
        PortRole::InOut => "inout",
    });
    hash.u64(node);
}

fn encode_executable_op(hash: &mut CanonicalHasher, op: &crate::hgraph::ExecutableOp) {
    use crate::hgraph::ExecutableOp;
    match op {
        ExecutableOp::Store => hash.tag("store"),
        ExecutableOp::LoadBinding => hash.tag("load-binding"),
        ExecutableOp::Invoke { fn_name, mode } => {
            hash.tag("invoke");
            hash.field(fn_name.as_bytes());
            encode_invoke_mode(hash, *mode);
        }
        ExecutableOp::EvalBackend { lang, env } => {
            hash.tag("eval-backend");
            hash.field(lang.as_bytes());
            hash.u32(*env);
        }
        ExecutableOp::InlineBackend { lang } => {
            hash.tag("inline-backend");
            hash.field(lang.as_bytes());
        }
        ExecutableOp::ForceRequest { kind } => {
            hash.tag("force-request");
            hash.field(kind.as_bytes());
        }
        ExecutableOp::Request { kind } => {
            hash.tag("request");
            hash.field(kind.as_bytes());
        }
        ExecutableOp::Group { mode } => {
            hash.tag("group");
            encode_group_mode(hash, *mode);
        }
        ExecutableOp::Schedule { kind } => {
            hash.tag("schedule");
            hash.field(kind.as_bytes());
        }
        ExecutableOp::MaterializeProject => hash.tag("materialize-project"),
        ExecutableOp::BuildRoute { route_id } => {
            hash.tag("build-route");
            hash.field(route_id.as_bytes());
        }
        ExecutableOp::RunRoute { route_id } => {
            hash.tag("run-route");
            hash.field(route_id.as_bytes());
        }
        ExecutableOp::SelectRoute { policy } => {
            hash.tag("select-route");
            hash.field(policy.as_bytes());
        }
        ExecutableOp::CompareRouteResults => hash.tag("compare-route-results"),
    }
}

fn encode_op_kind(hash: &mut CanonicalHasher, kind: &OpKind) {
    match kind {
        OpKind::Additive => hash.tag("additive"),
        OpKind::Multiplicative => hash.tag("multiplicative"),
        OpKind::Bitwise => hash.tag("bitwise"),
        OpKind::Ordered => hash.tag("ordered"),
        OpKind::Bounded { value } => {
            hash.tag("bounded");
            hash.field(value.to_string().as_bytes());
        }
        OpKind::AbiFixed { dom, rep } => {
            hash.tag("abi-fixed");
            hash.u16(dom.bits());
            hash.u16(rep.bits());
        }
        OpKind::Dereferenceable => hash.tag("dereferenceable"),
        OpKind::FieldAccess { field } => {
            hash.tag("field-access");
            hash.field(field.as_bytes());
        }
        OpKind::DataFlow => hash.tag("data-flow"),
        OpKind::StructuralBarrier => hash.tag("structural-barrier"),
        OpKind::Sequence => hash.tag("sequence"),
        OpKind::ActorSerial { actor } => {
            hash.tag("actor-serial");
            hash.u32(actor.lang);
            hash.u32(actor.env);
        }
        OpKind::Batch => hash.tag("batch"),
        OpKind::All => hash.tag("all"),
        OpKind::Any => hash.tag("any"),
        OpKind::Race => hash.tag("race"),
        OpKind::Request { kind } => {
            hash.tag("request");
            hash.field(kind.as_bytes());
        }
        OpKind::Schedule { kind } => {
            hash.tag("schedule");
            hash.field(kind.as_bytes());
        }
        OpKind::CacheMemo { cacheable } => {
            hash.tag("cache-memo");
            hash.bool(*cacheable);
        }
        OpKind::BackendCrossing { from_lang, to_lang } => {
            hash.tag("backend-crossing");
            hash.field(from_lang.as_bytes());
            hash.field(to_lang.as_bytes());
        }
        OpKind::X86 { mnemonic } => {
            hash.tag("x86");
            hash.field(mnemonic.as_bytes());
        }
        OpKind::OcoreOp { kind } => {
            hash.tag("ocore-op");
            encode_ocore_op(hash, kind);
        }
    }
}

fn encode_ocore_op(hash: &mut CanonicalHasher, kind: &OcoreOpKind) {
    match kind {
        OcoreOpKind::Add => hash.tag("add"),
        OcoreOpKind::Sub => hash.tag("sub"),
        OcoreOpKind::Mul => hash.tag("mul"),
        OcoreOpKind::Div => hash.tag("div"),
        OcoreOpKind::Load => hash.tag("load"),
        OcoreOpKind::Store => hash.tag("store"),
        OcoreOpKind::Inb => hash.tag("inb"),
        OcoreOpKind::Outb => hash.tag("outb"),
        OcoreOpKind::VolatileLoad => hash.tag("volatile-load"),
        OcoreOpKind::VolatileStore => hash.tag("volatile-store"),
        OcoreOpKind::AtomicFetch { order } => {
            hash.tag("atomic-fetch");
            hash.tag(match order {
                MemOrder::Relaxed => "relaxed",
                MemOrder::Acquire => "acquire",
                MemOrder::Release => "release",
                MemOrder::AcqRel => "acq-rel",
                MemOrder::SeqCst => "seq-cst",
            });
        }
    }
}

fn encode_invoke_mode(hash: &mut CanonicalHasher, mode: InvokeMode) {
    match mode {
        InvokeMode::Eager => hash.tag("eager"),
        InvokeMode::Lazy => hash.tag("lazy"),
        InvokeMode::Autonomous => hash.tag("autonomous"),
        InvokeMode::Group(mode) => {
            hash.tag("group");
            encode_group_mode(hash, mode);
        }
    }
}

fn encode_group_mode(hash: &mut CanonicalHasher, mode: GroupMode) {
    hash.tag(match mode {
        GroupMode::Batch => "batch",
        GroupMode::All => "all",
        GroupMode::Any => "any",
        GroupMode::Race => "race",
    });
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

struct CanonicalHasher(Sha256);

impl CanonicalHasher {
    fn new(domain: &str) -> Self {
        let mut hash = Sha256::new();
        hash.update((domain.len() as u64).to_be_bytes());
        hash.update(domain.as_bytes());
        Self(hash)
    }

    fn field(&mut self, bytes: &[u8]) {
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
    }

    fn tag(&mut self, tag: &str) {
        self.field(tag.as_bytes());
    }

    fn bool(&mut self, value: bool) {
        self.field(&[u8::from(value)]);
    }

    fn u16(&mut self, value: u16) {
        self.field(&value.to_be_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.field(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.field(&value.to_be_bytes());
    }

    fn usize(&mut self, value: usize) {
        self.u64(value as u64);
    }

    fn optional_u64(&mut self, value: Option<u64>) {
        match value {
            Some(value) => {
                self.tag("some");
                self.u64(value);
            }
            None => self.tag("none"),
        }
    }

    fn optional_usize(&mut self, value: Option<usize>) {
        self.optional_u64(value.map(|value| value as u64));
    }

    fn finish(self) -> String {
        hex::encode(self.0.finalize())
    }
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt as _;
    Cow::Borrowed(value.as_bytes())
}

#[cfg(windows)]
fn os_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    use std::os::windows::ffi::OsStrExt as _;
    let mut bytes = Vec::new();
    for unit in value.encode_wide() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    Cow::Owned(bytes)
}

#[cfg(not(any(unix, windows)))]
fn os_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    Cow::Owned(value.to_string_lossy().as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource_digest(resource: ResourceKey) -> String {
        let mut hash = CanonicalHasher::new("resource-key-test/v1");
        encode_resource_key(&mut hash, &resource);
        hash.finish()
    }

    #[test]
    fn exact_resource_digest_does_not_alias_display_equivalent_variants() {
        let exact_wildcard = ResourceKey::Network("*".to_string());
        let unknown = ResourceKey::NetworkUnknown;

        assert_eq!(exact_wildcard.to_string(), unknown.to_string());
        assert_ne!(resource_digest(exact_wildcard), resource_digest(unknown));
    }

    #[test]
    fn backend_binding_distinguishes_missing_from_present_non_regular_path() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("adapter.py");
        let missing = backend_artifact_state(&path);
        fs::create_dir(&path).unwrap();
        let non_regular = backend_artifact_state(&path);

        assert_eq!(missing, BackendArtifactStateV1::Missing);
        assert_eq!(non_regular, BackendArtifactStateV1::NonRegular);

        let runtime = |state| {
            build_runtime_binding(
                RuntimeSnapshotKindV1::Inspection,
                vec![BackendArtifactV1 {
                    canonical_backend: "python".to_string(),
                    resolved_identity: "path:/adapter.py".to_string(),
                    state,
                }],
                "catalog-projection-test".to_string(),
                &[("artifact-state-test", "v1")],
            )
        };
        assert_ne!(
            runtime(missing).backend_set_sha256(),
            runtime(non_regular).backend_set_sha256()
        );
    }

    fn program_for_backend(lang: &str, count: usize) -> OIrProgram {
        OIrProgram {
            nodes: (0..count)
                .map(|_| crate::ir::OIr::Exec {
                    lang: lang.to_string(),
                    env_id: u32::MAX,
                    attr: None,
                    backend: BackendRegistry::global().interface_for(lang),
                    body: vec![crate::ir::OIr::Text("catalog projection".to_string())],
                })
                .collect(),
        }
    }

    #[test]
    fn native_adapter_does_not_bind_a_same_named_legacy_shim_file() {
        let temp = tempfile::tempdir().unwrap();
        let shim_path = temp.path().join("bash_shim.py");
        fs::write(&shim_path, b"# unused native-backend decoy v1\n").unwrap();
        let program = program_for_backend("bash", 1);
        let plan = program.plan();
        let first = runtime_binding_from_directory(
            &plan,
            temp.path(),
            &[("adapter-binding-test", "native")],
        );

        assert_eq!(first.backend_artifacts().len(), 1);
        assert_eq!(
            first.backend_artifacts()[0].canonical_backend,
            "__ostadix_current_executable__"
        );

        fs::write(&shim_path, b"# unused native-backend decoy v2\n").unwrap();
        let second = runtime_binding_from_directory(
            &plan,
            temp.path(),
            &[("adapter-binding-test", "native")],
        );
        assert_eq!(first.backend_set_sha256(), second.backend_set_sha256());
    }

    #[test]
    fn legacy_python_adapter_binds_the_consumed_shim_file() {
        let temp = tempfile::tempdir().unwrap();
        let shim_path = temp.path().join("python_shim.py");
        fs::write(&shim_path, b"# consumed legacy shim v1\n").unwrap();
        let program = program_for_backend("python", 1);
        let plan = program.plan();
        let first = runtime_binding_from_directory(
            &plan,
            temp.path(),
            &[("adapter-binding-test", "legacy-python")],
        );

        assert!(first.backend_artifacts().iter().any(|artifact| {
            artifact.canonical_backend == "python"
                && matches!(artifact.state, BackendArtifactStateV1::Hashed { .. })
        }));

        fs::write(&shim_path, b"# consumed legacy shim v2\n").unwrap();
        let second = runtime_binding_from_directory(
            &plan,
            temp.path(),
            &[("adapter-binding-test", "legacy-python")],
        );
        assert_ne!(first.backend_set_sha256(), second.backend_set_sha256());
    }

    #[test]
    fn unknown_backend_retains_the_legacy_python_shim_fallback_binding() {
        let temp = tempfile::tempdir().unwrap();
        let shim_path = temp.path().join("research_backend_shim.py");
        fs::write(&shim_path, b"# extension shim\n").unwrap();
        let program = program_for_backend("research_backend", 1);
        let runtime = runtime_binding_from_directory(
            &program.plan(),
            temp.path(),
            &[("adapter-binding-test", "unknown")],
        );

        assert!(runtime.backend_artifacts().iter().any(|artifact| {
            artifact.canonical_backend == "research_backend"
                && matches!(artifact.state, BackendArtifactStateV1::Hashed { .. })
        }));
    }

    #[test]
    fn catalog_projection_is_plan_specific_canonical_and_capacity_neutral() {
        let python = program_for_backend("python", 1);
        let duplicate_python = program_for_backend("python", 2);
        let alias_python = program_for_backend("py", 1);
        let javascript = program_for_backend("javascript", 1);
        let context = &[("catalog-projection-test", "v1")];

        let snapshot = |program: &OIrProgram| {
            runtime_binding_from_adapter_bytes(&program.plan(), &[], context)
                .backend_catalog_projection_sha256()
                .to_string()
        };

        assert_eq!(snapshot(&python), snapshot(&duplicate_python));
        assert_eq!(snapshot(&python), snapshot(&alias_python));
        assert_ne!(snapshot(&python), snapshot(&javascript));
        assert_eq!(snapshot(&python).len(), 64);
    }

    #[test]
    fn analyzer_rejects_a_forged_catalog_projection_before_issuing_evidence() {
        let program = program_for_backend("python", 1);
        let plan = program.plan();
        let mut graph = program.hgraph();
        crate::hgraph::solve::solve_types(&mut graph).unwrap();
        let mut runtime = runtime_binding_from_adapter_bytes(
            &plan,
            &[],
            &[("catalog-projection-test", "forged")],
        );
        runtime.backend_catalog_projection_sha256 = "00".repeat(32);

        let error = analyze_execution(&program, &plan, &graph, runtime)
            .expect_err("a caller cannot substitute a catalog projection");
        assert!(
            error
                .to_string()
                .contains("backend catalog projection is stale"),
            "{error:#}"
        );
    }

    #[test]
    fn analyzer_rejects_a_duplicate_reserved_current_executable_artifact() {
        let program = OIrProgram {
            nodes: vec![crate::ir::OIr::Text("inert".to_string())],
        };
        let plan = program.plan();
        let mut graph = program.hgraph();
        crate::hgraph::solve::solve_types(&mut graph).unwrap();
        let runtime = build_runtime_binding(
            RuntimeSnapshotKindV1::Inspection,
            vec![BackendArtifactV1 {
                canonical_backend: "__ostadix_current_executable__".to_string(),
                resolved_identity: "forged:collision".to_string(),
                state: BackendArtifactStateV1::Hashed {
                    sha256: "00".repeat(32),
                },
            }],
            backend_catalog_projection_sha256(&plan),
            &[("reserved-artifact-test", "v1")],
        );

        let error = analyze_execution(&program, &plan, &graph, runtime).unwrap_err();
        assert!(error
            .to_string()
            .contains("exactly one reserved current-executable artifact"));
    }

    #[cfg(unix)]
    #[test]
    fn path_identity_preserves_non_utf8_bytes() {
        use std::os::unix::ffi::OsStringExt as _;

        let left = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![b'a', 0xff]));
        let right = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![b'a', 0xfe]));

        assert_eq!(left.to_string_lossy(), right.to_string_lossy());
        assert_ne!(path_identity(&left), path_identity(&right));
    }
}
