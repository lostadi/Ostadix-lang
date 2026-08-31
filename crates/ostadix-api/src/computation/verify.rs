//! Cross-check external artifact bytes against a verified computation.

use crate::computation_core::{FacetIdV1, OComputationErrorV1, VerifiedOComputationV1};

pub struct FacetBytesV1<'a> {
    pub id: FacetIdV1,
    pub bytes: &'a [u8],
}

/// Re-hash every supplied artifact and require exact manifest membership.
/// Missing manifest facets are rejected; manifest facets omitted by the
/// caller remain available for partial, explicitly bounded inspection.
pub fn verify_facet_bytes<'a>(
    computation: &VerifiedOComputationV1,
    facets: impl IntoIterator<Item = FacetBytesV1<'a>>,
) -> Result<(), OComputationErrorV1> {
    for facet in facets {
        computation.require_facet_bytes(&facet.id, facet.bytes)?;
    }
    Ok(())
}
