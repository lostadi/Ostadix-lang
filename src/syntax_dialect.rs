//! Narrow parser-facing projection of the backend catalog.
//!
//! Parsing needs only tag registration, canonical spelling, and whether a
//! backend owns quoted syntax. It must not inspect runtime availability,
//! purity, placement, authority, or any other execution capability.

/// The complete catalog view permitted at the syntax boundary.
pub trait SyntaxDialect {
    /// Whether `name` may begin a typed O expression in this parse.
    fn is_registered_syntax_tag(&self, name: &str) -> bool;

    /// Resolve a registered tag or alias to its canonical syntax name.
    fn canonical_syntax_name(&self, name: &str) -> String;

    /// Whether the canonical tag captures syntax instead of executable plan
    /// children. This controls source-origin suppression only.
    fn owns_quoted_syntax(&self, canonical_name: &str) -> bool;
}
