//! Shared primitives for process-local live capabilities.
//!
//! Serialized capability metadata is descriptive only. Live authority comes
//! from possession of a bearer identity that was generated from operating
//! system entropy and is still present in a private runtime binding table.

use anyhow::{Context, Result};

/// Generate a 256-bit bearer identity from the operating system CSPRNG.
///
/// The prefix names the authority domain but carries no authority itself. A
/// recipient must still resolve the full identity through the private table for
/// the current runtime or kernel session.
pub(crate) fn fresh_bearer_identity(prefix: &str) -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).context("failed to obtain entropy for capability identity")?;
    Ok(format!("{prefix}:{}", hex::encode(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_identities_use_256_bits_of_os_entropy() {
        let first = fresh_bearer_identity("test-live").unwrap();
        let second = fresh_bearer_identity("test-live").unwrap();
        assert!(first.starts_with("test-live:"));
        assert_eq!(first.len(), "test-live:".len() + 64);
        assert_ne!(first, second);
    }
}
