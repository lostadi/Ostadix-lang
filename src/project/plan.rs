//! Deterministic hosted project planning into the executable HGraph ontology.
//!
//! This is the bounded PR7 logical-planning surface. It constructs project
//! operations from a real [`ProjectBundle`] and the same resolved route policy
//! used by the hosted runtime, but it deliberately does not execute commands,
//! perform placement, or mint governed World authority.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::{Component, Path};

use sha2::{Digest, Sha256};

use crate::effects::{
    parse_declared_resource, EffectConfidence, EffectSummary, Fallibility, ResourceKey,
};
use crate::hgraph::{ExecutableOp, HEdgeKind, HGraph, HNode, HNodeKind, NodeId};
use crate::ir::PlanNodeId;
use crate::value::OValue;

use super::bundle;
use super::model::{
    ProjectBundle, RouteFailureContinuation, RouteGuard, RouteKind, RoutePolicy, RouteSpec,
};
use super::runtime::resolve_selection;

/// Policy-level cancellation/short-circuit behavior retained in the logical
/// plan until PR8 introduces separate runtime and recovery graphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectCancellationSemantics {
    None,
    StopAfterSuccess,
    CancelLosers,
}

impl ProjectCancellationSemantics {
    pub const fn token(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::StopAfterSuccess => "stop-after-success",
            Self::CancelLosers => "cancel-losers",
        }
    }
}

/// Stable route metadata carried by both route preparation and execution
/// operations. Environment values and command strings are bound by the bundle
/// digest but intentionally omitted from textual planner output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutePlanFacts {
    pub kind: RouteKind,
    /// Executable program named by the route, without arguments. This is a
    /// placement/runtime requirement, not proof that any provider has it.
    pub executable: Option<String>,
    /// Named O evaluator required by the route, when execution does not begin
    /// with an ordinary command.
    pub evaluator: Option<String>,
    /// Bundle-relative entrypoint retained for runtime/package matching.
    pub entrypoint: Option<String>,
    pub prerequisites: Vec<String>,
    pub guards: Vec<RouteGuard>,
    pub environment_keys: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub declared_reads: Vec<String>,
    pub declared_writes: Vec<String>,
    pub declared_pure: bool,
    pub failure_continuation: RouteFailureContinuation,
}

impl RoutePlanFacts {
    fn from_route(route: &RouteSpec) -> Self {
        Self {
            kind: route.kind,
            executable: route.command.first().cloned(),
            evaluator: route.evaluator.clone(),
            entrypoint: route.entrypoint.clone(),
            prerequisites: route.prerequisites.clone(),
            guards: route.guards.clone(),
            environment_keys: route.environment.keys().cloned().collect(),
            inputs: route.inputs.clone(),
            outputs: route.outputs.clone(),
            declared_reads: route.effects.reads.clone(),
            declared_writes: route.effects.writes.clone(),
            declared_pure: route.effects.pure,
            failure_continuation: route.failure_continuation,
        }
    }
}

/// One typed dependency on a prior project operation.
///
/// `Value` waits for the predecessor's ordinary result, including a settled
/// unsuccessful route result. `Success` waits for the predecessor's
/// successful-completion token, so a nonzero prerequisite cannot release its
/// dependent route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProjectDependency {
    Value(PlanNodeId),
    Success(PlanNodeId),
}

impl ProjectDependency {
    /// Return the planner-local operation that produces this dependency.
    pub const fn plan_node(self) -> PlanNodeId {
        match self {
            Self::Value(plan_node) | Self::Success(plan_node) => plan_node,
        }
    }

    const fn token(self) -> &'static str {
        match self {
            Self::Value(_) => "value",
            Self::Success(_) => "success",
        }
    }
}

/// One project operation and its typed logical predecessors. `id` is
/// planner-local; it is not an OIR `ExecutionPlan` identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectPlanOperation {
    pub id: PlanNodeId,
    pub op: ExecutableOp,
    pub dependencies: Vec<ProjectDependency>,
    pub effects: EffectSummary,
    /// Selected-alternative branch. It designates the isolated workspace used
    /// by the hosted runtime; residual HGraph resource chains remain global and
    /// conservative in this logical-planning slice.
    pub branch: Option<usize>,
    pub route_facts: Option<RoutePlanFacts>,
}

/// Exact logical project plan from which a project HGraph is projected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectExecutionPlan {
    pub project_name: String,
    /// SHA-256 over deterministic serialized `ProjectBundle` bytes. This binds
    /// routes, commands, guards, environment values, files, and policy data.
    pub bundle_digest: String,
    pub target: String,
    pub alternatives: Vec<String>,
    pub policy: RoutePolicy,
    pub cancellation: ProjectCancellationSemantics,
    pub operations: Vec<ProjectPlanOperation>,
    pub roots: Vec<PlanNodeId>,
}

/// A validated project plan paired with its exact HGraph projection.
#[derive(Debug)]
pub struct ProjectHGraph {
    pub plan: ProjectExecutionPlan,
    pub graph: HGraph,
}

impl ProjectExecutionPlan {
    /// Construct a project plan without materializing or executing the bundle.
    pub fn from_bundle(
        bundle: &ProjectBundle,
        target: Option<&str>,
        policy_override: Option<RoutePolicy>,
    ) -> Result<Self, String> {
        validate_bundle_structure(bundle)?;
        let selection = resolve_selection(bundle, target, policy_override)
            .map_err(|error| format!("failed to resolve project selection: {error}"))?;
        let bundle_digest = bundle_digest(bundle)?;
        let cancellation = cancellation_for(&selection.policy);
        let mut builder = PlanBuilder::new(bundle);
        let mut terminal_results = Vec::with_capacity(selection.alternatives.len());

        // The hosted route runtime materializes one isolated workspace for each
        // selected alternative. Shared prerequisites are therefore duplicated
        // across branches and memoized only within one branch.
        for (branch, route_id) in selection.alternatives.iter().enumerate() {
            let materialize = builder.push(
                ExecutableOp::MaterializeProject,
                Vec::new(),
                EffectSummary::unknown(),
                Some(branch),
                None,
            );
            let mut memo = HashMap::new();
            let terminal = builder.plan_route(branch, route_id, materialize, &mut memo)?;
            terminal_results.push(terminal);
        }

        let selection_dependencies = if selection.policy == RoutePolicy::VerifyEquivalent {
            let compare = builder.push(
                ExecutableOp::CompareRouteResults,
                terminal_results
                    .into_iter()
                    .map(ProjectDependency::Value)
                    .collect(),
                comparison_effects(),
                None,
                None,
            );
            vec![ProjectDependency::Value(compare)]
        } else {
            terminal_results
                .into_iter()
                .map(ProjectDependency::Value)
                .collect()
        };
        let select = builder.push(
            ExecutableOp::SelectRoute {
                policy: selection.policy.token(),
            },
            selection_dependencies,
            selection_effects(&selection.policy),
            None,
            None,
        );

        let plan = Self {
            project_name: bundle.name.clone(),
            bundle_digest,
            target: selection.target,
            alternatives: selection.alternatives,
            policy: selection.policy,
            cancellation,
            operations: builder.operations,
            roots: vec![select],
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Validate planner-local structure independently of the HGraph.
    pub fn validate(&self) -> Result<(), String> {
        if self.project_name.is_empty() {
            return Err("project plan has an empty project name".to_string());
        }
        if self.bundle_digest.len() != 64
            || !self
                .bundle_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("project plan has a non-canonical bundle digest".to_string());
        }
        if self.target.is_empty() {
            return Err("project plan has an empty selection target".to_string());
        }
        if self.alternatives.is_empty() {
            return Err("project plan has no selected alternatives".to_string());
        }
        let mut seen_alternatives = BTreeSet::new();
        for alternative in &self.alternatives {
            if alternative.is_empty() {
                return Err("project plan has an empty alternative route id".to_string());
            }
            if !seen_alternatives.insert(alternative.clone()) {
                return Err(format!(
                    "project plan repeats alternative route `{alternative}`"
                ));
            }
        }
        if let RoutePolicy::Explicit(route) = &self.policy {
            if route.is_empty() || self.alternatives != [route.clone()] {
                return Err(
                    "explicit project policy must name the sole selected alternative".to_string(),
                );
            }
        }
        if self.policy == RoutePolicy::Default && self.alternatives.len() != 1 {
            return Err(
                "default project policy must resolve to exactly one selected alternative"
                    .to_string(),
            );
        }
        let expected_cancellation = cancellation_for(&self.policy);
        if self.cancellation != expected_cancellation {
            return Err(format!(
                "project cancellation {} disagrees with policy {}",
                self.cancellation.token(),
                self.policy.token()
            ));
        }

        let mut materializations = BTreeMap::<usize, PlanNodeId>::new();
        let mut builds = BTreeMap::<(usize, String), PlanNodeId>::new();
        let mut runs = BTreeMap::<(usize, String), PlanNodeId>::new();
        let mut selection = None;
        let mut comparison = None;
        for (index, operation) in self.operations.iter().enumerate() {
            if operation.id != PlanNodeId(index) {
                return Err(format!(
                    "project operation index {index} has non-canonical id {}",
                    operation.id.0
                ));
            }
            let mut dependencies = BTreeSet::new();
            for dependency in &operation.dependencies {
                let predecessor = dependency.plan_node();
                if predecessor.0 >= index {
                    return Err(format!(
                        "project operation {} has non-preceding {} dependency {}",
                        operation.id.0,
                        dependency.token(),
                        predecessor.0
                    ));
                }
                if !dependencies.insert(*dependency) {
                    return Err(format!(
                        "project operation {} repeats {} dependency {}",
                        operation.id.0,
                        dependency.token(),
                        predecessor.0
                    ));
                }
            }
            match &operation.op {
                ExecutableOp::MaterializeProject => {
                    let branch = operation.branch.ok_or_else(|| {
                        format!(
                            "materialize operation {} has no alternative branch",
                            operation.id.0
                        )
                    })?;
                    if branch >= self.alternatives.len()
                        || operation.route_facts.is_some()
                        || !operation.dependencies.is_empty()
                        || operation.effects != EffectSummary::unknown()
                    {
                        return Err(format!(
                            "materialize operation {} has invalid branch, dependencies, metadata, or effects",
                            operation.id.0
                        ));
                    }
                    if materializations.insert(branch, operation.id).is_some() {
                        return Err(format!(
                            "project plan repeats materialization branch {branch}"
                        ));
                    }
                }
                ExecutableOp::BuildRoute { route_id } => {
                    let branch = operation.branch.ok_or_else(|| {
                        format!(
                            "build operation {} has no alternative branch",
                            operation.id.0
                        )
                    })?;
                    if branch >= self.alternatives.len()
                        || route_id.is_empty()
                        || operation.route_facts.is_none()
                        || operation.effects != EffectSummary::pure()
                    {
                        return Err(format!(
                            "build operation {} has invalid route metadata or effects",
                            operation.id.0
                        ));
                    }
                    if builds
                        .insert((branch, route_id.clone()), operation.id)
                        .is_some()
                    {
                        return Err(format!(
                            "project branch {branch} repeats build route `{route_id}`"
                        ));
                    }
                }
                ExecutableOp::RunRoute { route_id } => {
                    let branch = operation.branch.ok_or_else(|| {
                        format!("run operation {} has no alternative branch", operation.id.0)
                    })?;
                    let facts = operation.route_facts.as_ref().ok_or_else(|| {
                        format!("run operation {} lacks route metadata", operation.id.0)
                    })?;
                    if branch >= self.alternatives.len()
                        || route_id.is_empty()
                        || operation.effects != route_effects_from_facts(facts)?
                    {
                        return Err(format!(
                            "run operation {} has invalid route metadata or effects",
                            operation.id.0
                        ));
                    }
                    if runs
                        .insert((branch, route_id.clone()), operation.id)
                        .is_some()
                    {
                        return Err(format!(
                            "project branch {branch} repeats run route `{route_id}`"
                        ));
                    }
                }
                ExecutableOp::CompareRouteResults => {
                    if operation.branch.is_some()
                        || operation.route_facts.is_some()
                        || operation.effects != comparison_effects()
                    {
                        return Err(format!(
                            "comparison operation {} has invalid metadata or effects",
                            operation.id.0
                        ));
                    }
                    if comparison.replace(operation.id).is_some() {
                        return Err("project plan has multiple comparison operations".to_string());
                    }
                }
                ExecutableOp::SelectRoute { policy } => {
                    if policy != &self.policy.token()
                        || operation.branch.is_some()
                        || operation.route_facts.is_some()
                        || operation.effects != selection_effects(&self.policy)
                    {
                        return Err(format!(
                            "selection operation policy or metadata `{policy}` disagrees with `{}`",
                            self.policy.token()
                        ));
                    }
                    if selection.replace(operation.id).is_some() {
                        return Err("project plan has multiple selection operations".to_string());
                    }
                }
                other => {
                    return Err(format!(
                        "non-project operation {other:?} appears in a project plan"
                    ));
                }
            }
        }
        let expected_branches = (0..self.alternatives.len()).collect::<BTreeSet<_>>();
        if materializations.keys().copied().collect::<BTreeSet<_>>() != expected_branches {
            return Err(format!(
                "project plan materialization branches do not cover 0..{} exactly",
                self.alternatives.len()
            ));
        }
        if builds.keys().collect::<BTreeSet<_>>() != runs.keys().collect::<BTreeSet<_>>() {
            return Err("project plan build/run route inventories differ".to_string());
        }

        let mut terminal_results = Vec::with_capacity(self.alternatives.len());
        for (branch, alternative) in self.alternatives.iter().enumerate() {
            let materialize = materializations[&branch];
            let terminal = runs
                .get(&(branch, alternative.clone()))
                .copied()
                .ok_or_else(|| {
                    format!("project branch {branch} lacks terminal route `{alternative}`")
                })?;
            terminal_results.push(terminal);

            let mut reachable = BTreeSet::new();
            let mut pending = vec![alternative.clone()];
            while let Some(route_id) = pending.pop() {
                if !reachable.insert(route_id.clone()) {
                    continue;
                }
                let run_id = runs
                    .get(&(branch, route_id.clone()))
                    .copied()
                    .ok_or_else(|| {
                        format!("project branch {branch} lacks prerequisite route `{route_id}`")
                    })?;
                let build_id = builds[&(branch, route_id.clone())];
                let build = &self.operations[build_id.0];
                let run = &self.operations[run_id.0];
                if build.route_facts != run.route_facts {
                    return Err(format!(
                        "project branch {branch} route `{route_id}` build/run facts differ"
                    ));
                }
                if build.dependencies != [ProjectDependency::Value(materialize)] {
                    return Err(format!(
                        "project branch {branch} route `{route_id}` build does not depend only on its materialization"
                    ));
                }
                let facts = run
                    .route_facts
                    .as_ref()
                    .expect("run facts were checked above");
                let mut seen_prerequisites = BTreeSet::new();
                let mut expected_run_dependencies = vec![ProjectDependency::Value(build_id)];
                for prerequisite in &facts.prerequisites {
                    if prerequisite.is_empty() || !seen_prerequisites.insert(prerequisite.clone()) {
                        return Err(format!(
                            "project branch {branch} route `{route_id}` has an empty or repeated prerequisite"
                        ));
                    }
                    let prerequisite_run = runs
                        .get(&(branch, prerequisite.clone()))
                        .copied()
                        .ok_or_else(|| {
                            format!(
                                "project branch {branch} route `{route_id}` lacks prerequisite `{prerequisite}`"
                            )
                        })?;
                    expected_run_dependencies.push(ProjectDependency::Success(prerequisite_run));
                    pending.push(prerequisite.clone());
                }
                if run.dependencies != expected_run_dependencies {
                    return Err(format!(
                        "project branch {branch} route `{route_id}` run dependencies differ from its facts"
                    ));
                }
            }

            let branch_routes = runs
                .keys()
                .filter_map(|(candidate_branch, route)| {
                    (*candidate_branch == branch).then_some(route.clone())
                })
                .collect::<BTreeSet<_>>();
            if branch_routes != reachable {
                return Err(format!(
                    "project branch {branch} contains routes outside terminal `{alternative}` prerequisites"
                ));
            }
        }

        let select =
            selection.ok_or_else(|| "project plan has no selection operation".to_string())?;
        if select.0 + 1 != self.operations.len() {
            return Err("project selection operation is not terminal".to_string());
        }
        let select_operation = &self.operations[select.0];
        let terminal_values = terminal_results
            .iter()
            .copied()
            .map(ProjectDependency::Value)
            .collect::<Vec<_>>();
        if self.policy == RoutePolicy::VerifyEquivalent {
            let compare = comparison.ok_or_else(|| {
                "verify-equivalent project plan has no comparison operation".to_string()
            })?;
            if compare.0 + 1 != select.0
                || self.operations[compare.0].dependencies != terminal_values
                || select_operation.dependencies != [ProjectDependency::Value(compare)]
            {
                return Err(
                    "verify-equivalent comparison/selection topology is non-canonical".to_string(),
                );
            }
        } else if comparison.is_some() {
            return Err(format!(
                "project policy {} must not contain a comparison operation",
                self.policy.token()
            ));
        } else if select_operation.dependencies != terminal_values {
            return Err("project selection dependencies differ from terminal alternatives".into());
        }
        if self.roots != [select] {
            return Err("project plan root is not the selection result".to_string());
        }
        Ok(())
    }

    /// Stable logical-plan inspection text. Commands and environment values are
    /// bound by `bundle-sha256` but are not exposed here.
    pub fn to_text(&self) -> String {
        let mut output = String::from("; ProjectExecutionPlan\n");
        writeln!(
            output,
            "project name={} bundle-sha256={} operations={}",
            self.project_name,
            self.bundle_digest,
            self.operations.len()
        )
        .expect("writing to a String cannot fail");
        writeln!(
            output,
            "selection target={} policy={} alternatives=[{}] cancellation={} equivalence={}",
            self.target,
            self.policy.token(),
            self.alternatives.join(","),
            self.cancellation.token(),
            if self.policy == RoutePolicy::VerifyEquivalent {
                "required"
            } else {
                "none"
            }
        )
        .expect("writing to a String cannot fail");
        for operation in &self.operations {
            let dependencies = operation
                .dependencies
                .iter()
                .map(|dependency| format!("{}:p{}", dependency.token(), dependency.plan_node().0))
                .collect::<Vec<_>>()
                .join(",");
            let branch = operation
                .branch
                .map(|branch| branch.to_string())
                .unwrap_or_else(|| "-".to_string());
            writeln!(
                output,
                "project-op p{} kind={} branch={} deps=[{}] effects={}",
                operation.id.0,
                project_op_label(&operation.op),
                branch,
                dependencies,
                effect_label(&operation.effects)
            )
            .expect("writing to a String cannot fail");
            if let Some(facts) = &operation.route_facts {
                writeln!(
                    output,
                    "route-facts p{} kind={:?} prerequisites=[{}] guards=[{}] env=[{}] inputs=[{}] outputs=[{}] declared-reads=[{}] declared-writes=[{}] declared-pure={} failure-continuation={}",
                    operation.id.0,
                    facts.kind,
                    facts.prerequisites.join(","),
                    facts.guards.iter().map(guard_label).collect::<Vec<_>>().join(","),
                    facts.environment_keys.join(","),
                    facts.inputs.join(","),
                    facts.outputs.join(","),
                    facts.declared_reads.join(","),
                    facts.declared_writes.join(","),
                    facts.declared_pure,
                    facts.failure_continuation.token(),
                )
                .expect("writing to a String cannot fail");
            }
        }
        output
    }

    /// Project this plan into the existing HGraph ontology and prove that the
    /// projection contains exactly the plan's operations and dependencies.
    pub fn to_hgraph(&self) -> Result<HGraph, String> {
        self.validate()?;
        let graph = self.project_unvalidated()?;
        graph.validate_execution_graph()?;
        self.validate_projection(&graph)?;
        Ok(graph)
    }

    /// Construct the one canonical graph representation after planner-local
    /// validation. Keeping this separate lets `validate_projection` compare
    /// the entire graph inventory without recursively calling `to_hgraph`.
    fn project_unvalidated(&self) -> Result<HGraph, String> {
        let mut graph = HGraph::default();
        let bundle_value = graph.add_node(HNode::with_value(OValue::text(format!(
            "project-bundle-sha256:{}",
            self.bundle_digest
        ))));
        let mut values = HashMap::<PlanNodeId, NodeId>::new();
        let mut resource_frontiers = BTreeMap::<ResourceKey, ProjectResourceFrontier>::new();

        for operation in &self.operations {
            let value = graph.add_node(HNode::fresh());
            graph.node_mut(value).expect("fresh node exists").plan_node = Some(operation.id);
            let completion = graph.add_completion_node(operation.id)?;
            graph.set_effect_summary(operation.id, operation.effects.clone());

            let mut inputs = Vec::with_capacity(operation.dependencies.len() + 1);
            for dependency in &operation.dependencies {
                inputs.push(project_dependency_node(&graph, &values, *dependency)?);
            }
            if matches!(operation.op, ExecutableOp::MaterializeProject) {
                inputs.push(bundle_value);
            }
            let mut outputs = vec![value, completion];
            add_resource_transitions(
                &mut graph,
                &mut resource_frontiers,
                &operation.effects,
                operation.id,
                completion,
                &mut inputs,
                &mut outputs,
            );
            deduplicate_nodes(&mut inputs);
            deduplicate_nodes(&mut outputs);
            let stable_order = u64::try_from(operation.id.0).map_err(|_| {
                format!(
                    "project operation id {} does not fit the HGraph stable-order field",
                    operation.id.0
                )
            })?;
            graph.add_exec_edge(
                operation.id,
                operation.op.clone(),
                inputs,
                outputs,
                value,
                stable_order,
            )?;
            values.insert(operation.id, value);
        }
        for root in &self.roots {
            graph.push_root(values[root]);
        }
        Ok(graph)
    }

    /// Validate exact plan-to-HGraph projection. Generic HGraph validation is
    /// intentionally insufficient because its embedded source plan is OIR-only.
    pub fn validate_projection(&self, graph: &HGraph) -> Result<(), String> {
        self.validate()?;
        graph.validate_execution_graph()?;
        if graph.op_map.len() != self.operations.len()
            || graph.effect_summaries.len() != self.operations.len()
        {
            return Err("project HGraph operation/effect cardinality differs from its plan".into());
        }
        if !graph.ir_map.is_empty()
            || !graph.sequence_dependencies.is_empty()
            || !graph.bindings.is_empty()
        {
            return Err("project HGraph carries unrelated OIR/sequence/binding provenance".into());
        }

        let bundle_literals = graph
            .nodes
            .iter()
            .filter_map(|(id, node)| {
                (node.producer.is_none()
                    && node.is_value()
                    && node.value
                        == Some(OValue::text(format!(
                            "project-bundle-sha256:{}",
                            self.bundle_digest
                        ))))
                .then_some(*id)
            })
            .collect::<Vec<_>>();
        if bundle_literals.len() != 1 {
            return Err(format!(
                "project HGraph requires one exact bundle literal, found {}",
                bundle_literals.len()
            ));
        }
        let bundle_literal = bundle_literals[0];

        let mut values = HashMap::new();
        for operation in &self.operations {
            let info = graph.op_for(operation.id).ok_or_else(|| {
                format!("project operation {} has no HGraph edge", operation.id.0)
            })?;
            let edge = graph
                .exec_edge(info.edge)
                .ok_or_else(|| format!("project operation {} edge is missing", operation.id.0))?;
            if edge.op != HEdgeKind::Execute(operation.op.clone()) {
                return Err(format!(
                    "project operation {} semantics differ from its plan",
                    operation.id.0
                ));
            }
            if graph.effect_summary(operation.id) != Some(&operation.effects) {
                return Err(format!(
                    "project operation {} effects differ from its plan",
                    operation.id.0
                ));
            }
            let value = graph.node(info.value_output).ok_or_else(|| {
                format!(
                    "project operation {} value output is missing",
                    operation.id.0
                )
            })?;
            if value.plan_node != Some(operation.id) {
                return Err(format!(
                    "project operation {} value output has source identity {:?}",
                    operation.id.0, value.plan_node
                ));
            }
            values.insert(operation.id, info.value_output);
        }

        for operation in &self.operations {
            let info = graph
                .op_for(operation.id)
                .expect("operation existence was checked above");
            let mut expected = operation
                .dependencies
                .iter()
                .map(|dependency| project_dependency_node(graph, &values, *dependency))
                .collect::<Result<BTreeSet<_>, _>>()?;
            if matches!(operation.op, ExecutableOp::MaterializeProject) {
                expected.insert(bundle_literal);
            }
            let actual = info
                .inputs
                .iter()
                .filter(|node| {
                    graph.node(**node).is_some_and(|node| {
                        matches!(&node.kind, HNodeKind::Value | HNodeKind::Completion { .. })
                    })
                })
                .copied()
                .collect::<BTreeSet<_>>();
            if actual != expected {
                return Err(format!(
                    "project operation {} typed dependencies {:?} differ from {:?}",
                    operation.id.0, actual, expected
                ));
            }
        }
        let expected_roots = self
            .roots
            .iter()
            .map(|root| values[root])
            .collect::<Vec<_>>();
        if graph.root_nodes != expected_roots {
            return Err("project HGraph roots differ from its project plan".to_string());
        }
        let canonical = self.project_unvalidated()?;
        if graph != &canonical {
            return Err(
                "project HGraph node, edge, state, or provenance inventory differs from the canonical projection"
                    .to_string(),
            );
        }
        Ok(())
    }
}

impl ProjectHGraph {
    /// Reconstruct the canonical project plan and reject bundle, route, or
    /// policy substitution even if the supplied HGraph is otherwise valid.
    pub fn validate_source(
        &self,
        bundle: &ProjectBundle,
        target: Option<&str>,
        policy_override: Option<RoutePolicy>,
    ) -> Result<(), String> {
        let canonical = ProjectExecutionPlan::from_bundle(bundle, target, policy_override)?;
        if self.plan != canonical {
            return Err("project plan does not match the supplied bundle and selection".into());
        }
        self.plan.validate_projection(&self.graph)
    }

    pub fn to_text(&self) -> String {
        format!(
            "{}\n{}",
            self.plan.to_text(),
            self.graph.to_execution_text()
        )
    }
}

/// Build and validate a real project HGraph without executing project code.
pub fn build_project_hgraph(
    bundle: &ProjectBundle,
    target: Option<&str>,
    policy_override: Option<RoutePolicy>,
) -> Result<ProjectHGraph, String> {
    let plan = ProjectExecutionPlan::from_bundle(bundle, target, policy_override.clone())?;
    let graph = plan.to_hgraph()?;
    let project = ProjectHGraph { plan, graph };
    project.validate_source(bundle, target, policy_override)?;
    Ok(project)
}

struct PlanBuilder<'a> {
    bundle: &'a ProjectBundle,
    operations: Vec<ProjectPlanOperation>,
}

impl<'a> PlanBuilder<'a> {
    fn new(bundle: &'a ProjectBundle) -> Self {
        Self {
            bundle,
            operations: Vec::new(),
        }
    }

    fn push(
        &mut self,
        op: ExecutableOp,
        dependencies: Vec<ProjectDependency>,
        effects: EffectSummary,
        branch: Option<usize>,
        route_facts: Option<RoutePlanFacts>,
    ) -> PlanNodeId {
        let id = PlanNodeId(self.operations.len());
        self.operations.push(ProjectPlanOperation {
            id,
            op,
            dependencies,
            effects,
            branch,
            route_facts,
        });
        id
    }

    fn plan_route(
        &mut self,
        branch: usize,
        route_id: &str,
        materialize: PlanNodeId,
        memo: &mut HashMap<String, PlanNodeId>,
    ) -> Result<PlanNodeId, String> {
        if let Some(existing) = memo.get(route_id) {
            return Ok(*existing);
        }
        let route = self
            .bundle
            .route(route_id)
            .ok_or_else(|| format!("project route `{route_id}` does not exist"))?;
        let mut prerequisite_results = Vec::with_capacity(route.prerequisites.len());
        for prerequisite in &route.prerequisites {
            prerequisite_results.push(self.plan_route(branch, prerequisite, materialize, memo)?);
        }
        let facts = RoutePlanFacts::from_route(route);
        let build = self.push(
            ExecutableOp::BuildRoute {
                route_id: route_id.to_string(),
            },
            vec![ProjectDependency::Value(materialize)],
            EffectSummary::pure(),
            Some(branch),
            Some(facts.clone()),
        );
        let mut run_dependencies = vec![ProjectDependency::Value(build)];
        run_dependencies.extend(
            prerequisite_results
                .into_iter()
                .map(ProjectDependency::Success),
        );
        let run = self.push(
            ExecutableOp::RunRoute {
                route_id: route_id.to_string(),
            },
            run_dependencies,
            route_effects(route)?,
            Some(branch),
            Some(facts),
        );
        memo.insert(route_id.to_string(), run);
        Ok(run)
    }
}

fn validate_bundle_structure(bundle: &ProjectBundle) -> Result<(), String> {
    let mut route_ids = BTreeSet::new();
    for route in &bundle.routes {
        if route.id.is_empty() {
            return Err("project bundle contains an empty route id".to_string());
        }
        if !route_ids.insert(route.id.clone()) {
            return Err(format!("project bundle repeats route id `{}`", route.id));
        }
        validate_project_path(&route.working_directory, "working directory")?;
        for path in &route.inputs {
            validate_project_path(path, "route input")?;
        }
        for path in &route.outputs {
            validate_project_path(path, "route output")?;
        }
        // Parse every declared effect now, including unselected routes, so a
        // malformed or governed source spelling cannot hide outside the plan.
        route_effects(route)?;
    }
    if let Some(default) = &bundle.default_route {
        if !route_ids.contains(default) {
            return Err(format!(
                "project default route `{default}` does not name a route"
            ));
        }
    }

    let mut set_names = BTreeSet::new();
    for set in &bundle.route_sets {
        if set.provides.is_empty() {
            return Err("project bundle contains an unnamed route set".to_string());
        }
        if route_ids.contains(&set.provides) {
            return Err(format!(
                "route set `{}` conflicts with a route id",
                set.provides
            ));
        }
        if !set_names.insert(set.provides.clone()) {
            return Err(format!(
                "project bundle repeats route set `{}`",
                set.provides
            ));
        }
        if set.alternatives.is_empty() {
            return Err(format!("route set `{}` has no alternatives", set.provides));
        }
        let mut alternatives = BTreeSet::new();
        for route in &set.alternatives {
            if !alternatives.insert(route.clone()) {
                return Err(format!(
                    "route set `{}` repeats alternative `{route}`",
                    set.provides
                ));
            }
            if !route_ids.contains(route) {
                return Err(format!(
                    "route set `{}` references missing route `{route}`",
                    set.provides
                ));
            }
        }
    }

    for route in &bundle.routes {
        for prerequisite in &route.prerequisites {
            if !route_ids.contains(prerequisite) {
                return Err(format!(
                    "route `{}` references missing prerequisite `{prerequisite}`",
                    route.id
                ));
            }
        }
    }
    validate_prerequisite_acyclicity(bundle)
}

fn validate_prerequisite_acyclicity(bundle: &ProjectBundle) -> Result<(), String> {
    fn visit(
        bundle: &ProjectBundle,
        route_id: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if visited.contains(route_id) {
            return Ok(());
        }
        if !visiting.insert(route_id.to_string()) {
            return Err(format!(
                "project route prerequisite cycle reaches `{route_id}`"
            ));
        }
        let route = bundle
            .route(route_id)
            .ok_or_else(|| format!("project route `{route_id}` does not exist"))?;
        for prerequisite in &route.prerequisites {
            visit(bundle, prerequisite, visiting, visited)?;
        }
        visiting.remove(route_id);
        visited.insert(route_id.to_string());
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for route in &bundle.routes {
        visit(bundle, &route.id, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn bundle_digest(bundle: &ProjectBundle) -> Result<String, String> {
    let bytes = bundle::serialize(bundle)
        .map_err(|error| format!("failed to serialize project bundle for planning: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn cancellation_for(policy: &RoutePolicy) -> ProjectCancellationSemantics {
    match policy {
        RoutePolicy::Fallback | RoutePolicy::AnySuccess => {
            ProjectCancellationSemantics::StopAfterSuccess
        }
        RoutePolicy::RaceSuccess | RoutePolicy::RaceSettle => {
            ProjectCancellationSemantics::CancelLosers
        }
        _ => ProjectCancellationSemantics::None,
    }
}

fn comparison_effects() -> EffectSummary {
    let mut summary = EffectSummary::pure();
    summary.fallibility = Fallibility::MayFail;
    summary
}

fn selection_effects(policy: &RoutePolicy) -> EffectSummary {
    let mut summary = EffectSummary::pure();
    summary.fallibility = Fallibility::MayFail;
    if matches!(
        policy,
        RoutePolicy::RaceSuccess | RoutePolicy::RaceSettle | RoutePolicy::BenchmarkAndSelect
    ) {
        summary.deterministic = false;
        summary.confidence = EffectConfidence::Conservative;
    }
    summary
}

fn route_effects(route: &RouteSpec) -> Result<EffectSummary, String> {
    route_effects_from_facts(&RoutePlanFacts::from_route(route))
}

fn route_effects_from_facts(facts: &RoutePlanFacts) -> Result<EffectSummary, String> {
    // A user `pure=true` declaration is descriptive metadata, not proof that a
    // hosted command is deterministic, infallible, mediated, or worker-safe.
    let mut summary = EffectSummary::unknown();
    summary.spawn = true;
    for path in &facts.inputs {
        validate_project_path(path, "route input")?;
        summary.reads.insert(ResourceKey::ProjectPath(path.clone()));
    }
    for path in &facts.outputs {
        validate_project_path(path, "route output")?;
        summary
            .writes
            .insert(ResourceKey::ProjectPath(path.clone()));
    }
    for guard in &facts.guards {
        if let RouteGuard::EnvVarSet(name) = guard {
            summary
                .reads
                .insert(parse_declared_resource(&format!("env:{name}"))?);
        }
    }
    for resource in &facts.declared_reads {
        add_declared_resources(&mut summary.reads, resource)?;
    }
    for resource in &facts.declared_writes {
        add_declared_resources(&mut summary.writes, resource)?;
    }
    summary.network = summary
        .reads
        .iter()
        .chain(summary.writes.iter())
        .any(|resource| {
            matches!(
                resource,
                ResourceKey::Network(_) | ResourceKey::NetworkUnknown
            )
        });
    Ok(summary)
}

fn add_declared_resources(
    destination: &mut BTreeSet<ResourceKey>,
    declaration: &str,
) -> Result<(), String> {
    if declaration.trim().is_empty() {
        return Err("project route contains an empty resource declaration".to_string());
    }
    for resource in declaration.split(['+', ';']).map(str::trim) {
        if resource.is_empty() {
            return Err(format!("empty resource in `{declaration}`"));
        }
        destination.insert(parse_declared_resource(resource)?);
    }
    Ok(())
}

fn validate_project_path(path: &str, label: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    let path = Path::new(path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "{label} `{}` must remain project-relative",
            path.display()
        ));
    }
    Ok(())
}

fn add_resource_transitions(
    graph: &mut HGraph,
    frontiers: &mut BTreeMap<ResourceKey, ProjectResourceFrontier>,
    summary: &EffectSummary,
    producer: PlanNodeId,
    completion: NodeId,
    inputs: &mut Vec<NodeId>,
    outputs: &mut Vec<NodeId>,
) {
    let (reads, writes) = summary.scheduling_accesses();
    let resources = reads.union(&writes).cloned().collect::<BTreeSet<_>>();
    for resource in resources {
        let frontier = frontiers.entry(resource.clone()).or_insert_with(|| {
            let initial = graph.add_node(HNode::resource_state(resource.clone(), 0));
            ProjectResourceFrontier {
                last_write: initial,
                version: 0,
                open_reads: BTreeSet::new(),
            }
        });
        inputs.push(frontier.last_write);
        if writes.contains(&resource) {
            inputs.extend(std::mem::take(&mut frontier.open_reads));
            let next_version = frontier.version + 1;
            let successor = graph.add_node(HNode::resource_state(resource.clone(), next_version));
            graph
                .node_mut(successor)
                .expect("fresh resource node exists")
                .plan_node = Some(producer);
            frontier.last_write = successor;
            frontier.version = next_version;
            outputs.push(successor);
        } else {
            frontier.open_reads.insert(completion);
        }
    }
}

struct ProjectResourceFrontier {
    last_write: NodeId,
    version: u64,
    open_reads: BTreeSet<NodeId>,
}

fn project_dependency_node(
    graph: &HGraph,
    values: &HashMap<PlanNodeId, NodeId>,
    dependency: ProjectDependency,
) -> Result<NodeId, String> {
    let predecessor = dependency.plan_node();
    match dependency {
        ProjectDependency::Value(_) => values.get(&predecessor).copied().ok_or_else(|| {
            format!(
                "project value dependency {} has no preceding ordinary output",
                predecessor.0
            )
        }),
        ProjectDependency::Success(_) => graph.completion_node(predecessor).ok_or_else(|| {
            format!(
                "project success dependency {} has no preceding completion output",
                predecessor.0
            )
        }),
    }
}

fn deduplicate_nodes(nodes: &mut Vec<NodeId>) {
    let mut seen = BTreeSet::new();
    nodes.retain(|node| seen.insert(*node));
}

fn project_op_label(op: &ExecutableOp) -> String {
    match op {
        ExecutableOp::MaterializeProject => "materialize-project".to_string(),
        ExecutableOp::BuildRoute { route_id } => format!("build-route:{route_id}"),
        ExecutableOp::RunRoute { route_id } => format!("run-route:{route_id}"),
        ExecutableOp::SelectRoute { policy } => format!("select-route:{policy}"),
        ExecutableOp::CompareRouteResults => "compare-route-results".to_string(),
        other => format!("invalid:{other:?}"),
    }
}

fn effect_label(summary: &EffectSummary) -> String {
    let reads = summary
        .reads
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let writes = summary
        .writes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "reads=[{reads}] writes=[{writes}] unknown={} spawn={} deterministic={} fallibility={:?} confidence={:?}",
        summary.unknown,
        summary.spawn,
        summary.deterministic,
        summary.fallibility,
        summary.confidence
    )
}

fn guard_label(guard: &RouteGuard) -> String {
    match guard {
        RouteGuard::PlatformOs(os) => format!("os:{os}"),
        RouteGuard::CommandAvailable(command) => format!("command:{command}"),
        RouteGuard::EnvVarSet(name) => format!("env:{name}"),
    }
}
