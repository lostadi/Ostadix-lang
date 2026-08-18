//! Generic discoverer — a placeholder hook that finds nothing automatically.
//!
//! It exists so CLI route declarations and manifest routes have a home in the
//! discoverer chain and so future heuristics can be added without touching the
//! orchestration.

use std::path::Path;

use crate::project::discover::EcosystemDiscoverer;
use crate::project::model::{ProjectFile, RouteSpec};

pub struct GenericDiscoverer;

impl EcosystemDiscoverer for GenericDiscoverer {
    fn name(&self) -> &str {
        "generic"
    }

    fn discover(&self, _root: &Path, _files: &[ProjectFile]) -> Vec<RouteSpec> {
        Vec::new()
    }
}
