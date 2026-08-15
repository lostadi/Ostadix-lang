//! Environment identity at the V5/V6 compatibility boundary.
//!
//! V5 stores an environment as a `u32` on `ONode`/`OIr`.  V6 keeps that wire
//! shape readable while assigning the two reserved high values explicit
//! meanings.  New code should reason through [`EnvironmentRefV2`] instead of
//! comparing raw integers.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Historical spelling for a bare block such as `python^(...)_python`.
pub const EPHEMERAL_ENV_ID: u32 = u32::MAX;

/// Additive V6 spelling for a linker-created fresh environment: `python[*]`.
///
/// The runtime normalizes this to an ephemeral process instance at dispatch,
/// but retaining a distinct encoded value preserves source intent in OIR,
/// fingerprints, diagnostics, and round-trip reconstruction.
pub const LINKER_ISOLATED_ENV_ID: u32 = u32::MAX - 1;

/// Largest numeric environment identity available to authored persistent
/// environments.  The two values above it are reserved protocol sentinels.
pub const MAX_PERSISTENT_ENV_ID: u32 = u32::MAX - 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum EnvironmentRefV2 {
    /// A bare source block.  Every dispatch gets a fresh evaluator instance.
    Ephemeral,
    /// A fresh evaluator requested explicitly by generated/linker source.
    LinkerIsolated,
    /// Stable logical evaluator identity authored as `[N]`.
    Persistent(u32),
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum EnvironmentIdError {
    #[error(
        "persistent environment id {0} is reserved; numeric ids must be at most {MAX_PERSISTENT_ENV_ID}"
    )]
    Reserved(u32),
}

impl EnvironmentRefV2 {
    pub fn persistent(id: u32) -> Result<Self, EnvironmentIdError> {
        if id > MAX_PERSISTENT_ENV_ID {
            return Err(EnvironmentIdError::Reserved(id));
        }
        Ok(Self::Persistent(id))
    }

    /// Decode the unchanged V5 `u32` storage field.
    pub const fn from_encoded(id: u32) -> Self {
        match id {
            EPHEMERAL_ENV_ID => Self::Ephemeral,
            LINKER_ISOLATED_ENV_ID => Self::LinkerIsolated,
            persistent => Self::Persistent(persistent),
        }
    }

    /// Encode into the unchanged V5 `u32` storage field.
    pub const fn encoded(self) -> u32 {
        match self {
            Self::Ephemeral => EPHEMERAL_ENV_ID,
            Self::LinkerIsolated => LINKER_ISOLATED_ENV_ID,
            Self::Persistent(id) => id,
        }
    }

    /// Physical process-registry key.  Both fresh forms deliberately collapse
    /// to the registry's ephemeral sentinel because neither may be reused.
    pub const fn runtime_env_id(self) -> u32 {
        match self {
            Self::Ephemeral | Self::LinkerIsolated => EPHEMERAL_ENV_ID,
            Self::Persistent(id) => id,
        }
    }

    pub const fn is_fresh(self) -> bool {
        matches!(self, Self::Ephemeral | Self::LinkerIsolated)
    }

    pub const fn is_persistent(self) -> bool {
        matches!(self, Self::Persistent(_))
    }

    /// Optional source marker between the backend name and `^(`.
    pub fn source_marker(self) -> Option<String> {
        match self {
            Self::Ephemeral => None,
            Self::LinkerIsolated => Some("[*]".to_string()),
            Self::Persistent(id) => Some(format!("[{id}]")),
        }
    }
}

impl fmt::Display for EnvironmentRefV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ephemeral => formatter.write_str("ephemeral"),
            Self::LinkerIsolated => formatter.write_str("linker-isolated"),
            Self::Persistent(id) => write!(formatter, "persistent:{id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_compatibility_is_total_and_round_trips() {
        for environment in [
            EnvironmentRefV2::Ephemeral,
            EnvironmentRefV2::LinkerIsolated,
            EnvironmentRefV2::Persistent(0),
            EnvironmentRefV2::Persistent(MAX_PERSISTENT_ENV_ID),
        ] {
            assert_eq!(
                EnvironmentRefV2::from_encoded(environment.encoded()),
                environment
            );
        }
    }

    #[test]
    fn fresh_environments_share_physical_ephemeral_semantics_only() {
        assert_ne!(EPHEMERAL_ENV_ID, LINKER_ISOLATED_ENV_ID);
        assert_eq!(
            EnvironmentRefV2::LinkerIsolated.runtime_env_id(),
            EPHEMERAL_ENV_ID
        );
        assert!(EnvironmentRefV2::LinkerIsolated.is_fresh());
        assert!(!EnvironmentRefV2::LinkerIsolated.is_persistent());
    }

    #[test]
    fn numeric_source_cannot_claim_reserved_sentinels() {
        assert_eq!(
            EnvironmentRefV2::persistent(LINKER_ISOLATED_ENV_ID),
            Err(EnvironmentIdError::Reserved(LINKER_ISOLATED_ENV_ID))
        );
        assert_eq!(
            EnvironmentRefV2::persistent(EPHEMERAL_ENV_ID),
            Err(EnvironmentIdError::Reserved(EPHEMERAL_ENV_ID))
        );
    }
}
