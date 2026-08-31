//! Ordinary `.O` projection into the computation identity spine.

use anyhow::{bail, Context, Result};

use crate::computation_core::{
    ComputationLineageId, ComputationTokenV1, DerivationInputV1, DerivationRefV1,
    DerivationRelationV1, FacetIdV1, FacetKindV1, FacetRefV1, TransformIdentityV1,
    VerifiedOComputationV1,
};
use crate::evidence::ExecutionIntentV1;
use crate::execution_contract::Policy;
use crate::hgraph::HGraph;
use crate::ir::{BackendRegistry, ExecutionPlan, OIrProgram};
use crate::parser::Parser;
use crate::resource_identity::ArtifactId;

use super::OComputationBuilderV1;

const SOURCE_SCHEMA_V1: &str = "ostadix.source/o/v1";
const OIR_SCHEMA_V1: &str = "ostadix.oir-program/v1";
const EXECUTION_PLAN_SCHEMA_V1: &str = "ostadix.execution-plan/v1";
const SOLVED_HGRAPH_SCHEMA_V1: &str = "ostadix.solved-hgraph/v1";

fn id(value: &str) -> Result<FacetIdV1> {
    Ok(FacetIdV1::new(value)?)
}

fn schema(value: &str) -> Result<ComputationTokenV1> {
    Ok(ComputationTokenV1::new(value)?)
}

fn digest(value: &str) -> Result<ArtifactId> {
    Ok(ArtifactId::from_sha256(value.to_owned())?)
}

fn descriptor(name: &str, bytes: &[u8]) -> Result<TransformIdentityV1> {
    Ok(TransformIdentityV1::from_descriptor(name, bytes)?)
}

fn input(role: &str, facet: FacetIdV1) -> Result<DerivationInputV1> {
    Ok(DerivationInputV1::new(
        ComputationTokenV1::new(role)?,
        facet,
    ))
}

fn strip_shebang(source: &str) -> &str {
    if source.starts_with("#!") {
        source
            .find('\n')
            .map(|newline| &source[newline + 1..])
            .unwrap_or_default()
    } else {
        source
    }
}

/// Bind the exact ordinary-O semantic products already checked by one
/// `ExecutionIntentV1`. Runtime discovery, evidence, admission, and dispatch
/// remain fresh later operations and are intentionally absent here.
pub fn build_oir_computation_v1(
    lineage: ComputationLineageId,
    source: &[u8],
    program: &OIrProgram,
    plan: &ExecutionPlan,
    solved_graph: &HGraph,
    intent: &ExecutionIntentV1,
) -> Result<VerifiedOComputationV1> {
    let source_text = std::str::from_utf8(source).context("ordinary-O source is not UTF-8")?;
    let backends = BackendRegistry::global().registered_backend_tags();
    let nodes = Parser::new(strip_shebang(source_text), &backends)
        .parse()
        .context("could not parse ordinary-O source for custody verification")?;
    let lowered = OIrProgram::lower(&nodes);
    if &lowered != program {
        bail!("supplied ordinary-O source does not lower to the supplied OIR program");
    }

    intent
        .validate()
        .context("invalid ordinary-O execution intent")?;
    let policy = Policy::from_name(&intent.base_policy)
        .with_context(|| format!("unsupported intent policy `{}`", intent.base_policy))?;
    let expected = ExecutionIntentV1::compile(source, program, plan, solved_graph, policy)
        .context("could not recompute ordinary-O semantic custody")?;
    if &expected != intent {
        bail!("execution intent does not describe the supplied source, OIR, plan, and graph");
    }

    let source_id = id("source")?;
    let oir_id = id("oir-program")?;
    let plan_id = id("execution-plan")?;
    let graph_id = id("solved-hgraph")?;
    let intent_id = id("execution-intent")?;

    let mut builder = OComputationBuilderV1::new(lineage);
    builder
        .add_root_facet(FacetRefV1::new(
            source_id.clone(),
            FacetKindV1::Source,
            schema(SOURCE_SCHEMA_V1)?,
            digest(&intent.source_sha256)?,
        ))
        .add_facet(FacetRefV1::new(
            oir_id.clone(),
            FacetKindV1::OirProgram,
            schema(OIR_SCHEMA_V1)?,
            digest(&intent.oir_sha256)?,
        ))
        .add_facet(FacetRefV1::new(
            plan_id.clone(),
            FacetKindV1::ExecutionPlan,
            schema(EXECUTION_PLAN_SCHEMA_V1)?,
            digest(&intent.plan_sha256)?,
        ))
        .add_facet(FacetRefV1::new(
            graph_id.clone(),
            FacetKindV1::SolvedHgraph,
            schema(SOLVED_HGRAPH_SCHEMA_V1)?,
            digest(&intent.analyzed_graph_sha256)?,
        ))
        .add_facet(FacetRefV1::new(
            intent_id.clone(),
            FacetKindV1::ExecutionIntent,
            schema(&intent.schema)?,
            digest(&intent.execution_intent_sha256)?,
        ))
        .add_derivation(DerivationRefV1::new(
            DerivationRelationV1::LoweredFrom,
            vec![input("source", source_id.clone())?],
            oir_id.clone(),
            descriptor("ostadix/oir-lowering/v1", b"ostadix-lowered-oir-source/v1")?,
        ))
        .add_derivation(DerivationRefV1::new(
            DerivationRelationV1::PlannedFrom,
            vec![input("oir", oir_id.clone())?],
            plan_id.clone(),
            descriptor("ostadix/execution-planner/v1", b"ostadix-execution-plan/v1")?,
        ))
        .add_derivation(DerivationRefV1::new(
            DerivationRelationV1::SolvedFrom,
            vec![
                input("oir", oir_id.clone())?,
                input("plan", plan_id.clone())?,
            ],
            graph_id.clone(),
            descriptor(
                "ostadix/hgraph-solver/v1",
                b"ostadix-solved-executable-hgraph/v1",
            )?,
        ))
        .add_derivation(DerivationRefV1::new(
            DerivationRelationV1::AnalyzedFrom,
            vec![
                input("source", source_id)?,
                input("oir", oir_id)?,
                input("plan", plan_id)?,
                input("graph", graph_id)?,
            ],
            intent_id,
            TransformIdentityV1::new(
                ComputationTokenV1::new(intent.analyzer_id.clone())?,
                digest(&intent.analyzer_sha256)?,
            ),
        ));
    Ok(builder.finish()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hgraph::solve::solve_types;
    use crate::ir::OIr;

    #[test]
    fn exact_ordinary_products_share_one_revision() {
        let source = b"stable";
        let program = OIrProgram {
            nodes: vec![OIr::Text("stable".to_string())],
        };
        let plan = program.plan();
        let mut graph = program.hgraph_for_plan(&plan).unwrap();
        solve_types(&mut graph).unwrap();
        let intent =
            ExecutionIntentV1::compile(source, &program, &plan, &graph, Policy::Eager).unwrap();
        let lineage = ComputationLineageId::new("tests/ordinary").unwrap();
        let first =
            build_oir_computation_v1(lineage.clone(), source, &program, &plan, &graph, &intent)
                .unwrap();
        let second =
            build_oir_computation_v1(lineage, source, &program, &plan, &graph, &intent).unwrap();
        assert_eq!(first.revision(), second.revision());
    }

    #[test]
    fn substituted_source_is_rejected() {
        let source = b"stable";
        let program = OIrProgram {
            nodes: vec![OIr::Text("stable".to_string())],
        };
        let plan = program.plan();
        let mut graph = program.hgraph_for_plan(&plan).unwrap();
        solve_types(&mut graph).unwrap();
        let intent =
            ExecutionIntentV1::compile(source, &program, &plan, &graph, Policy::Eager).unwrap();
        let error = build_oir_computation_v1(
            ComputationLineageId::new("tests/substitution").unwrap(),
            b"text^(changed)_text",
            &program,
            &plan,
            &graph,
            &intent,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("does not lower"), "{error}");
    }

    #[test]
    fn self_consistent_intent_over_forged_lowering_is_rejected() {
        let source = b"actual source";
        let forged_program = OIrProgram {
            nodes: vec![OIr::Text("forged OIR".to_string())],
        };
        let forged_plan = forged_program.plan();
        let mut forged_graph = forged_program.hgraph_for_plan(&forged_plan).unwrap();
        solve_types(&mut forged_graph).unwrap();
        let forged_intent = ExecutionIntentV1::compile(
            source,
            &forged_program,
            &forged_plan,
            &forged_graph,
            Policy::Eager,
        )
        .unwrap();

        let error = build_oir_computation_v1(
            ComputationLineageId::new("tests/forged-lowering").unwrap(),
            source,
            &forged_program,
            &forged_plan,
            &forged_graph,
            &forged_intent,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("does not lower"), "{error}");
    }
}
