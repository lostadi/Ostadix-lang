//! Integration entrypoints binding placement protocol validation to the
//! process's compiled backend catalog.
//!
//! This implementation lives above both protocol and registry storage so the
//! placement protocol remains registry-independent.

use crate::placement::protocol::{
    CandidateDecisionV1, NodeProfileV1, PlacementCandidateInputV1, PlacementValidationError,
    RecordAuthenticatorV1, TargetDescriptorV1, UnixMillisV1,
};
use crate::registry::bundle::BackendRegistry;

impl TargetDescriptorV1 {
    pub fn validate_current_backend_catalog(&self) -> Result<(), PlacementValidationError> {
        self.validate_current_backend_catalog_with(BackendRegistry::global())
    }
}

impl NodeProfileV1 {
    pub fn validate_at(
        &self,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> Result<(), PlacementValidationError> {
        self.validate_at_with_catalog(now, authenticator, BackendRegistry::global())
    }
}

impl PlacementCandidateInputV1<'_> {
    pub fn evaluate(
        &self,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> CandidateDecisionV1 {
        self.evaluate_with_catalog(now, authenticator, BackendRegistry::global())
    }
}
