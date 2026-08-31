//! High-level builders for authority-free [`OComputation`](crate::computation_core)
//! manifests.
//!
//! Domain modules remain owners of their native representations. These
//! builders only attach immutable facet identities and witnessed derivations
//! to the low-level computation spine.

use crate::computation_core::{
    artifact_id_for_bytes, ComputationLineageId, ComputationRevisionId, DerivationRefV1, FacetIdV1,
    FacetKindV1, FacetRefV1, OComputationErrorV1, OComputationManifestV1, VerifiedOComputationV1,
};

pub mod build_oir;
pub mod build_project;
pub mod verify;

/// Incremental manifest assembly with verification deferred to `finish`.
/// This builder carries no execution authority and performs no dispatch.
#[derive(Debug)]
pub struct OComputationBuilderV1 {
    manifest: OComputationManifestV1,
}

impl OComputationBuilderV1 {
    pub fn new(lineage: ComputationLineageId) -> Self {
        Self {
            manifest: OComputationManifestV1::new(lineage),
        }
    }

    pub fn add_parent(&mut self, parent: ComputationRevisionId) -> &mut Self {
        self.manifest.parents.push(parent);
        self
    }

    pub fn add_facet(&mut self, facet: FacetRefV1) -> &mut Self {
        self.manifest.facets.push(facet);
        self
    }

    pub fn add_root_facet(&mut self, facet: FacetRefV1) -> &mut Self {
        self.manifest.roots.push(facet.id.clone());
        self.add_facet(facet)
    }

    pub fn add_facet_bytes(
        &mut self,
        id: FacetIdV1,
        kind: FacetKindV1,
        schema: crate::computation_core::ComputationTokenV1,
        bytes: &[u8],
    ) -> &mut Self {
        self.add_facet(FacetRefV1::new(
            id,
            kind,
            schema,
            artifact_id_for_bytes(bytes),
        ))
    }

    pub fn add_derivation(&mut self, derivation: DerivationRefV1) -> &mut Self {
        self.manifest.derivations.push(derivation);
        self
    }

    pub fn finish(self) -> Result<VerifiedOComputationV1, OComputationErrorV1> {
        self.manifest.verify()
    }
}
