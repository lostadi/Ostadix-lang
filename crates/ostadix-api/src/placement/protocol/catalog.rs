use super::{BackendImplementationIdV1, BackendStateSupportV2, SemanticDigestV1};

/// Current-catalog authority injected into transport-independent validation.
///
/// A digest can remain structurally inspectable after a catalog rollover while
/// no longer authorizing placement. Implementations therefore expose the
/// current authorization set explicitly instead of letting protocol records
/// reach into a process-global registry.
pub trait CurrentBackendCatalogV1 {
    fn current_schema(&self) -> &str;

    fn contains_current_specification(&self, digest: &SemanticDigestV1) -> bool;

    /// Whether the complete backend implementation was minted with the
    /// current catalog and realization formula. A current specification alone
    /// is insufficient after an executable-set or realization-domain rollover.
    fn contains_current_implementation(&self, implementation: &BackendImplementationIdV1) -> bool;

    fn state_support_for_current_specification(
        &self,
        digest: &SemanticDigestV1,
    ) -> Option<&BackendStateSupportV2>;
}
