//! Actor identities for the graph executor.
//!
//! Every backend operation runs "as" some actor. Persistent environments
//! (`env_id != u32::MAX`) map to a stable actor keyed by `(language,
//! environment)` so that repeated operations against the same environment are
//! serialized in stable ordinal order. Ephemeral blocks (`env_id ==
//! u32::MAX`) each get a UNIQUE `ephemeral_instance` so that two unrelated
//! ephemeral computations never serialize against one another.

use std::collections::HashMap;

use crate::ir::{ExecutionPlan, PlanNodeId, PlanNodeKind};

/// A concrete actor identity. Two operations may only run concurrently if
/// their actor keys differ.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActorKey {
    pub language: String,
    pub environment: u32,
    /// Unique per ephemeral block; `None` for persistent environments.
    pub ephemeral_instance: Option<u64>,
    /// Bumped whenever a backend process for this actor is torn down after a
    /// protocol error, so post-failure operations observe a fresh generation.
    pub generation: u64,
}

impl ActorKey {
    /// Whether this actor is an ephemeral (single-shot) backend instance.
    pub fn is_ephemeral(&self) -> bool {
        self.ephemeral_instance.is_some()
    }

    /// The persistent identity `(language, environment)` this actor serializes
    /// against. Ephemeral actors serialize only against themselves.
    pub fn persistent_id(&self) -> Option<(String, u32)> {
        if self.is_ephemeral() {
            None
        } else {
            Some((self.language.clone(), self.environment))
        }
    }

    pub fn describe(&self) -> String {
        match self.ephemeral_instance {
            Some(instance) => format!("{}[*ephemeral#{instance}*]", self.language),
            None => format!("{}[{}]", self.language, self.environment),
        }
    }
}

/// Assigns actor identities to the backend operations of a plan. Ephemeral
/// blocks receive monotonically increasing instance ids in stable plan order.
#[derive(Debug, Default)]
pub struct ActorTable {
    by_node: HashMap<PlanNodeId, ActorKey>,
}

impl ActorTable {
    /// Build the actor table for a plan, consulting `generation_of` for the
    /// current per-`(lang, env)` generation of persistent environments.
    pub fn build(plan: &ExecutionPlan, generation_of: impl Fn(&str, u32) -> u64) -> Self {
        let mut by_node = HashMap::new();
        let mut next_ephemeral: u64 = 0;
        for node in &plan.nodes {
            if let PlanNodeKind::Exec { lang, env_id, .. } = &node.kind {
                let key = if *env_id == u32::MAX {
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
