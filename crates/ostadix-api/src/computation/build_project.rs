//! Project projection into the computation identity spine.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::computation_core::{
    artifact_id_for_bytes, ComputationLineageId, ComputationTokenV1, DerivationInputV1,
    DerivationRefV1, DerivationRelationV1, FacetIdV1, FacetKindV1, FacetRefV1, TransformIdentityV1,
    VerifiedOComputationV1,
};
use crate::project::{bundle, DeploymentPlanV1, LogicalHGraphV1, ProjectBundle, ProjectHGraph};
use crate::resource_identity::ArtifactId;

use super::OComputationBuilderV1;

fn id(value: &str) -> Result<FacetIdV1> {
    Ok(FacetIdV1::new(value)?)
}

fn schema(value: &str) -> Result<ComputationTokenV1> {
    Ok(ComputationTokenV1::new(value)?)
}

fn transform(name: &str, descriptor: &[u8]) -> Result<TransformIdentityV1> {
    Ok(TransformIdentityV1::from_descriptor(name, descriptor)?)
}

fn input(role: &str, facet: FacetIdV1) -> Result<DerivationInputV1> {
    Ok(DerivationInputV1::new(
        ComputationTokenV1::new(role)?,
        facet,
    ))
}

/// Bind one exact project bundle, validated project HGraph, logical graph, and
/// hosted deployment proposal without manufacturing placement or authority.
pub fn build_project_computation_v1(
    lineage: ComputationLineageId,
    bundle_record: &ProjectBundle,
    project: &ProjectHGraph,
    logical: &LogicalHGraphV1,
    deployment: &DeploymentPlanV1,
) -> Result<VerifiedOComputationV1> {
    let bundle_bytes =
        bundle::serialize(bundle_record).context("could not serialize the exact project bundle")?;
    let bundle_digest = hex::encode(Sha256::digest(&bundle_bytes));
    project
        .validate_source(
            bundle_record,
            Some(&project.plan.target),
            Some(project.plan.policy.clone()),
        )
        .map_err(anyhow::Error::msg)
        .context("project HGraph does not derive from the supplied bundle and selection")?;
    let expected_logical = LogicalHGraphV1::from_project(project)
        .context("could not reconstruct the logical project graph")?;
    if &expected_logical != logical {
        bail!("logical graph does not describe the supplied project HGraph");
    }
    deployment
        .validate_trusted_hosted(logical)
        .context("deployment does not describe the supplied logical graph")?;

    let bundle_id = id("project-bundle")?;
    let project_graph_id = id("project-hgraph")?;
    let logical_id = id("logical-hgraph")?;
    let deployment_id = id("deployment")?;
    let project_graph_bytes = project.to_text().into_bytes();

    let mut builder = OComputationBuilderV1::new(lineage);
    builder
        .add_root_facet(FacetRefV1::new(
            bundle_id.clone(),
            FacetKindV1::ProjectBundle,
            schema("ostadix.project-bundle/v2")?,
            ArtifactId::from_sha256(bundle_digest)?,
        ))
        .add_facet(FacetRefV1::new(
            project_graph_id.clone(),
            FacetKindV1::SolvedHgraph,
            schema("ostadix.project-hgraph/v1")?,
            artifact_id_for_bytes(&project_graph_bytes),
        ))
        .add_facet(FacetRefV1::new(
            logical_id.clone(),
            FacetKindV1::LogicalHgraph,
            schema("ostadix.world.logical-hgraph/v1")?,
            logical.digest()?,
        ))
        .add_facet(FacetRefV1::new(
            deployment_id.clone(),
            FacetKindV1::Deployment,
            schema("ostadix.world.deployment-plan/v1")?,
            deployment.digest()?,
        ))
        .add_derivation(DerivationRefV1::new(
            DerivationRelationV1::ProjectedFrom,
            vec![input("bundle", bundle_id)?],
            project_graph_id.clone(),
            transform(
                "ostadix/project-hgraph-builder/v1",
                b"ostadix-project-hgraph/v1",
            )?,
        ))
        .add_derivation(DerivationRefV1::new(
            DerivationRelationV1::ProjectedFrom,
            vec![input("project_graph", project_graph_id)?],
            logical_id.clone(),
            transform(
                "ostadix/logical-projector/v1",
                b"ostadix.world.logical-hgraph/v1",
            )?,
        ))
        .add_derivation(DerivationRefV1::new(
            DerivationRelationV1::PlannedFrom,
            vec![input("logical_graph", logical_id)?],
            deployment_id,
            transform(
                "ostadix/hosted-deployment-planner/v1",
                b"ostadix.world.deployment-plan/v1",
            )?,
        ));
    Ok(builder.finish()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{build_project_hgraph, RouteProvenance, RouteSpec};

    fn fixture_bundle() -> ProjectBundle {
        let mut bundle = ProjectBundle::empty("computation-project");
        let mut route = RouteSpec::new(
            "main",
            RouteProvenance::Manifest {
                path: "tests/ocomputation".to_owned(),
            },
        );
        route.command = vec!["true".to_owned()];
        route.is_default = true;
        bundle.routes.push(route);
        bundle.default_route = Some("main".to_owned());
        bundle
    }

    #[test]
    fn exact_project_inputs_are_revision_stable_and_substitution_is_rejected() {
        let bundle = fixture_bundle();
        let project = build_project_hgraph(&bundle, Some("main"), None).unwrap();
        let logical = LogicalHGraphV1::from_project(&project).unwrap();
        let deployment = DeploymentPlanV1::hosted(&logical).unwrap();
        let lineage = ComputationLineageId::new("tests/project-builder").unwrap();

        let first =
            build_project_computation_v1(lineage.clone(), &bundle, &project, &logical, &deployment)
                .unwrap();
        let second =
            build_project_computation_v1(lineage.clone(), &bundle, &project, &logical, &deployment)
                .unwrap();
        assert_eq!(first.revision(), second.revision());
        assert_eq!(
            first.canonical_bytes().unwrap(),
            second.canonical_bytes().unwrap()
        );

        let mut substituted_bundle = bundle.clone();
        substituted_bundle
            .metadata
            .insert("substituted".to_owned(), "true".to_owned());
        let error = build_project_computation_v1(
            lineage.clone(),
            &substituted_bundle,
            &project,
            &logical,
            &deployment,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("project HGraph does not derive from the supplied bundle and selection"),
            "{error}"
        );

        let mut forged_plan = project.plan.clone();
        forged_plan.project_name = "forged-project-name".to_owned();
        let forged_graph = forged_plan.to_hgraph().unwrap();
        let forged_project = ProjectHGraph {
            plan: forged_plan,
            graph: forged_graph,
        };
        let error = build_project_computation_v1(
            lineage.clone(),
            &bundle,
            &forged_project,
            &logical,
            &deployment,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("project HGraph does not derive from the supplied bundle and selection"),
            "{error}"
        );

        let substituted_project =
            build_project_hgraph(&substituted_bundle, Some("main"), None).unwrap();
        let substituted_logical = LogicalHGraphV1::from_project(&substituted_project).unwrap();
        let error = build_project_computation_v1(
            lineage.clone(),
            &bundle,
            &project,
            &substituted_logical,
            &deployment,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("logical graph does not describe the supplied project HGraph"),
            "{error}"
        );

        let substituted_deployment = DeploymentPlanV1::hosted(&substituted_logical).unwrap();
        let error = build_project_computation_v1(
            lineage,
            &bundle,
            &project,
            &logical,
            &substituted_deployment,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("deployment does not describe the supplied logical graph"),
            "{error}"
        );
    }
}
