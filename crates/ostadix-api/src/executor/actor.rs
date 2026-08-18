//! Compatibility actor identities for plan analysis.
//!
//! This table gives backend plan nodes stable labels for diagnostics and older
//! analysis callers. It is not a second production scheduler: executable graph
//! readiness is controlled by explicit resource-state and completion-token
//! nodes. Persistent environment serialization is represented there by
//! `ResourceKey::ActorState`; distinct ephemeral labels alone make no
//! concurrency guarantee.

use std::collections::HashMap;

use crate::environment::{EnvironmentRefV2, LINKER_ISOLATED_ENV_ID};
use crate::ir::{ExecutionPlan, PlanNodeId, PlanNodeKind};

/// A concrete actor label used by compatibility analysis.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActorKey {
    pub language: String,
    pub environment: u32,
    /// Unique per ephemeral block; `None` for persistent environments.
    pub ephemeral_instance: Option<u64>,
    /// Opaque generation metadata supplied by the caller. `ActorTable` does
    /// not inspect backend processes or infer process generations.
    pub generation: u64,
}

impl ActorKey {
    /// Whether this actor is an ephemeral (single-shot) backend instance.
    pub fn is_ephemeral(&self) -> bool {
        self.ephemeral_instance.is_some()
    }

    /// The persistent `(language, environment)` identity represented by this
    /// label. Ephemeral labels have no persistent identity.
    pub fn persistent_id(&self) -> Option<(String, u32)> {
        if self.is_ephemeral() {
            None
        } else {
            Some((self.language.clone(), self.environment))
        }
    }

    pub fn describe(&self) -> String {
        match self.ephemeral_instance {
            Some(instance) if self.environment == LINKER_ISOLATED_ENV_ID => {
                format!("{}[*linked#{instance}*]", self.language)
            }
            Some(instance) => format!("{}[*ephemeral#{instance}*]", self.language),
            None => format!("{}[{}]", self.language, self.environment),
        }
    }
}

/// Assigns compatibility actor labels to backend operations in stable plan
/// order. These labels do not participate in production readiness decisions.
#[derive(Debug, Default)]
pub struct ActorTable {
    by_node: HashMap<PlanNodeId, ActorKey>,
}

impl ActorTable {
    /// Build the actor table, copying caller-provided generation metadata for
    /// persistent `(language, environment)` labels.
    pub fn build(plan: &ExecutionPlan, generation_of: impl Fn(&str, u32) -> u64) -> Self {
        let mut by_node = HashMap::new();
        let mut next_ephemeral: u64 = 0;
        for node in &plan.nodes {
            if let PlanNodeKind::Exec { lang, env_id, .. } = &node.kind {
                let key = if EnvironmentRefV2::from_encoded(*env_id).is_fresh() {
                    let instance = next_ephemeral;
                    next_ephemeral += 1;
                    ActorKey {
                        language: lang.clone(),
                        environment: *env_id,
                        ephemeral_instance: Some(instance),
                        generation: 0,
                    }
                } else {
                    ActorKey {
                        language: lang.clone(),
                        environment: *env_id,
                        ephemeral_instance: None,
                        generation: generation_of(lang, *env_id),
                    }
                };
                by_node.insert(node.id, key);
            }
        }
        Self { by_node }
    }

    pub fn actor_for(&self, node: PlanNodeId) -> Option<&ActorKey> {
        self.by_node.get(&node)
    }
}
