//! Shared primitives for process-local live capabilities.
//!
//! Serialized capability metadata is descriptive only. Live authority comes
//! from possession of a bearer identity that was generated from operating
//! system entropy and is still present in a private runtime binding table.

use std::collections::{BTreeSet, HashMap};

use anyhow::{bail, Context, Result};

use crate::value::{BackendAuthority, CapabilityKind, OValue};

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

/// Immutable authority policy attached to one live backend process.
///
/// A persistent `(language, environment)` process is additionally keyed by
/// this policy, so a process started for a more privileged block is never
/// reused by a later block with fewer rights.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BackendSandboxPolicy {
    permissions: Vec<BackendAuthority>,
}

impl BackendSandboxPolicy {
    pub(crate) fn new(permissions: impl IntoIterator<Item = BackendAuthority>) -> Self {
        let permissions = permissions.into_iter().collect::<BTreeSet<_>>();
        Self {
            permissions: permissions.into_iter().collect(),
        }
    }

    #[cfg(test)]
    pub(crate) fn none() -> Self {
        Self::new([])
    }

    #[allow(dead_code)]
    pub(crate) fn contains(&self, permission: BackendAuthority) -> bool {
        self.permissions.binary_search(&permission).is_ok()
    }

    pub(crate) fn names(&self) -> Vec<&'static str> {
        self.permissions
            .iter()
            .map(|permission| permission.name())
            .collect()
    }

    pub(crate) fn permissions(&self) -> &[BackendAuthority] {
        &self.permissions
    }
}

#[derive(Debug, Clone)]
struct BackendAuthorityBinding {
    language: String,
    permissions: BTreeSet<BackendAuthority>,
}

/// Process-local broker for hosted backend authority.
///
/// Capability metadata never populates this table. Only `issue` creates a
/// binding, and every direct or deferred shim dispatch resolves the bearer
/// through this table immediately before process execution.
#[derive(Default)]
pub(crate) struct BackendAuthorityBroker {
    bindings: HashMap<String, BackendAuthorityBinding>,
}

impl BackendAuthorityBroker {
    pub(crate) fn issue(
        &mut self,
        language: impl Into<String>,
        permissions: impl IntoIterator<Item = BackendAuthority>,
    ) -> Result<OValue> {
        let language = language.into();
        if language.is_empty() {
            bail!("backend execution capability requires a language");
        }
        let permissions = permissions.into_iter().collect::<BTreeSet<_>>();
        let identity = loop {
            let candidate = fresh_bearer_identity("o-backend-live")?;
            if !self.bindings.contains_key(&candidate) {
                break candidate;
            }
        };
        self.bindings.insert(
            identity.clone(),
            BackendAuthorityBinding {
                language: language.clone(),
                permissions: permissions.clone(),
            },
        );
        let metadata = HashMap::from([
            ("live".into(), OValue::bool_(true)),
            ("language".into(), OValue::str_(language)),
            (
                "permissions".into(),
                OValue::list(
                    permissions
                        .iter()
                        .map(|permission| OValue::str_(permission.name()))
                        .collect(),
                ),
            ),
        ]);
        Ok(OValue::capability(
            CapabilityKind::BackendExecution,
            identity,
            metadata,
        ))
    }

    pub(crate) fn authorize(
        &self,
        capability: &OValue,
        language: &str,
        requested: &[BackendAuthority],
    ) -> Result<String> {
        let OValue::Capability { kind, identity, .. } = capability else {
            bail!("expected OCapability, got {}", capability.type_name());
        };
        if *kind != CapabilityKind::BackendExecution {
            bail!(
                "expected a backend_execution capability, got {}",
                kind.name()
            );
        }
        self.authorize_identity(identity, language, requested)?;
        Ok(identity.clone())
    }

    pub(crate) fn authorize_identity(
        &self,
        identity: &str,
        language: &str,
        requested: &[BackendAuthority],
    ) -> Result<()> {
        let binding = self.bindings.get(identity).ok_or_else(|| {
            anyhow::anyhow!(
                "backend execution capability is forged, revoked, or from another evaluator"
            )
        })?;
        if binding.language != language && binding.language != "*" {
            bail!(
                "backend execution capability is scoped to {}, not {}",
                binding.language,
                language
            );
        }
        for permission in requested {
            if !binding.permissions.contains(permission) {
                bail!(
                    "backend execution capability for {} lacks `{}` authority",
                    language,
                    permission.name()
                );
            }
        }
        Ok(())
    }

    pub(crate) fn revoke(&mut self, capability: &OValue) -> Result<()> {
        let OValue::Capability { kind, identity, .. } = capability else {
            bail!("expected OCapability, got {}", capability.type_name());
        };
        if *kind != CapabilityKind::BackendExecution {
            bail!(
                "expected a backend_execution capability, got {}",
                kind.name()
            );
        }
        self.bindings.remove(identity).ok_or_else(|| {
            anyhow::anyhow!("backend execution capability is not live in this evaluator")
        })?;
        Ok(())
    }
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

    #[test]
    fn backend_broker_rejects_forged_wrong_language_and_missing_rights() {
        let mut broker = BackendAuthorityBroker::default();
        let capability = broker
            .issue("python", [BackendAuthority::FileRead])
            .unwrap();
        assert!(broker
            .authorize(&capability, "python", &[BackendAuthority::FileRead])
            .is_ok());
        assert!(broker
            .authorize(&capability, "python", &[BackendAuthority::Network])
            .is_err());
        assert!(broker.authorize(&capability, "javascript", &[]).is_err());

        let forged = OValue::capability(
            CapabilityKind::BackendExecution,
            "o-backend-live:forged",
            HashMap::new(),
        );
        assert!(broker.authorize(&forged, "python", &[]).is_err());
    }
}
