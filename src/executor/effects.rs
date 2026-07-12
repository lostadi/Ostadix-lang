//! Effect summaries for the graph executor.
//!
//! Each operation carries an [`EffectSummary`] describing the resources it
//! reads and writes plus whether it is deterministic. The coordinator uses
//! these summaries to decide whether two ready operations may run
//! concurrently: two operations conflict when they touch the same resource
//! with at least one write, when they share actor state, or when either side
//! is `unknown` and the other is `unknown`/host-global.
//!
//! Block-level effect declarations are parsed from the same attribute string
//! consumed by the block-options parser (`effects=`, `reads=`, `writes=`,
//! `serial=host`). Unknown attributes are ignored here so the existing block
//! attribute parser remains the single source of truth for hard errors.

use std::collections::BTreeSet;

use crate::executor::actor::ActorKey;

/// A named resource an operation may read or write.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceKey {
    ProjectPath(String),
    HostPath(String),
    EnvVar(String),
    Stdio,
    Network(String),
    NetworkUnknown,
    HostGlobal,
    Service(String),
    ActorState(String),
}

/// A conservative description of an operation's observable effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectSummary {
    pub deterministic: bool,
    pub reads: BTreeSet<ResourceKey>,
    pub writes: BTreeSet<ResourceKey>,
    pub actor_state: Option<ActorKey>,
    /// The operation's full effect footprint is not statically known.
    pub unknown: bool,
    pub network: bool,
    pub spawn: bool,
    pub clock: bool,
}

impl EffectSummary {
    /// A pure, deterministic, side-effect-free summary.
    pub fn pure() -> Self {
        Self {
            deterministic: true,
            reads: BTreeSet::new(),
            writes: BTreeSet::new(),
            actor_state: None,
            unknown: false,
            network: false,
            spawn: false,
            clock: false,
        }
    }

    /// A conservative "unknown/impure" summary: conflicts with any other
    /// unknown or host-global effect and with itself.
    pub fn unknown() -> Self {
        Self {
            deterministic: false,
            reads: BTreeSet::new(),
            writes: BTreeSet::new(),
            actor_state: None,
            unknown: true,
            network: false,
            spawn: false,
            clock: false,
        }
    }

    /// Attach an actor's serial state to the summary. Operations sharing this
    /// actor state conflict.
    pub fn with_actor_state(mut self, actor: ActorKey) -> Self {
        self.actor_state = Some(actor);
        self
    }

    fn touches_host_global(&self) -> bool {
        self.unknown
            || self.reads.contains(&ResourceKey::HostGlobal)
            || self.writes.contains(&ResourceKey::HostGlobal)
    }

    /// Whether two operations with these summaries conflict and therefore may
    /// not run concurrently.
    pub fn conflicts_with(&self, other: &EffectSummary) -> bool {
        // Same actor state always conflicts.
        if let (Some(a), Some(b)) = (&self.actor_state, &other.actor_state) {
            if a == b {
                return true;
            }
        }

        // Unknown effects conflict conservatively with any other unknown or
        // host-global effect.
        if (self.unknown && (other.unknown || other.touches_host_global()))
            || (other.unknown && (self.unknown || self.touches_host_global()))
        {
            return true;
        }

        // write vs read/write of the same resource conflicts; read/read does
        // not.
        if resource_conflict(&self.writes, &other.writes)
            || resource_conflict(&self.writes, &other.reads)
            || resource_conflict(&other.writes, &self.reads)
        {
            return true;
        }

        false
    }
}

fn resource_conflict(a: &BTreeSet<ResourceKey>, b: &BTreeSet<ResourceKey>) -> bool {
    a.iter().any(|resource| b.contains(resource))
}

/// A parsed block-level effect declaration overriding the default summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectDeclaration {
    /// `effects=pure` / `effects=unknown`.
    pub purity: Option<DeclaredPurity>,
    pub reads: BTreeSet<ResourceKey>,
    pub writes: BTreeSet<ResourceKey>,
    /// `serial=host` — force serialization against a shared host resource.
    pub serial_host: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclaredPurity {
    Pure,
    Unknown,
}

impl EffectDeclaration {
    /// Whether the attribute string carried any effect declaration at all.
    pub fn is_empty(&self) -> bool {
        self.purity.is_none() && self.reads.is_empty() && self.writes.is_empty() && !self.serial_host
    }

    /// Parse effect declarations out of the comma-separated block-attribute
    /// string. Resource lists are `+`-separated, e.g.
    /// `reads=project:src+host:/etc/hosts`. Non-effect attributes are ignored.
    pub fn parse(attr: Option<&str>) -> Self {
        let mut decl = EffectDeclaration::default();
        let Some(attr) = attr else {
            return decl;
        };
        for entry in attr.split(',') {
            let entry = entry.trim();
            if let Some(value) = entry.strip_prefix("effects=") {
                match value {
                    "pure" => decl.purity = Some(DeclaredPurity::Pure),
                    "unknown" => decl.purity = Some(DeclaredPurity::Unknown),
                    _ => {}
                }
            } else if let Some(value) = entry.strip_prefix("reads=") {
                decl.reads.extend(parse_resources(value));
            } else if let Some(value) = entry.strip_prefix("writes=") {
                decl.writes.extend(parse_resources(value));
            } else if let Some(value) = entry.strip_prefix("serial=") {
                if value == "host" {
                    decl.serial_host = true;
                }
            }
        }
        decl
    }

    /// Apply this declaration on top of a base summary derived from the
    /// backend, returning the effective summary.
    pub fn apply(&self, mut base: EffectSummary) -> EffectSummary {
        match self.purity {
            Some(DeclaredPurity::Pure) => {
                base.unknown = false;
                base.deterministic = true;
            }
            Some(DeclaredPurity::Unknown) => {
                base.unknown = true;
                base.deterministic = false;
            }
            None => {}
        }
        base.reads.extend(self.reads.iter().cloned());
        base.writes.extend(self.writes.iter().cloned());
        if self.serial_host {
            base.writes.insert(ResourceKey::HostGlobal);
        }
        base
    }
}

fn parse_resources(list: &str) -> Vec<ResourceKey> {
    list.split(['+', ';'])
        .filter_map(|item| {
            let item = item.trim();
            if item.is_empty() {
                return None;
            }
            Some(match item.split_once(':') {
                Some(("project", path)) => ResourceKey::ProjectPath(path.to_string()),
                Some(("host", path)) => ResourceKey::HostPath(path.to_string()),
                Some(("env", name)) => ResourceKey::EnvVar(name.to_string()),
                Some(("service", name)) => ResourceKey::Service(name.to_string()),
                Some(("network", host)) => ResourceKey::Network(host.to_string()),
                Some(("actor", name)) => ResourceKey::ActorState(name.to_string()),
                Some(("stdio", _)) | None if item == "stdio" => ResourceKey::Stdio,
                _ => ResourceKey::HostGlobal,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_read_does_not_conflict() {
        let mut a = EffectSummary::pure();
        a.reads.insert(ResourceKey::ProjectPath("src".into()));
        let mut b = EffectSummary::pure();
        b.reads.insert(ResourceKey::ProjectPath("src".into()));
        assert!(!a.conflicts_with(&b));
    }

    #[test]
    fn write_read_same_resource_conflicts() {
        let mut a = EffectSummary::pure();
        a.writes.insert(ResourceKey::ProjectPath("src".into()));
        let mut b = EffectSummary::pure();
        b.reads.insert(ResourceKey::ProjectPath("src".into()));
        assert!(a.conflicts_with(&b));
    }

    #[test]
    fn unknown_conflicts_with_unknown() {
        assert!(EffectSummary::unknown().conflicts_with(&EffectSummary::unknown()));
    }

    #[test]
    fn pure_does_not_conflict_with_unknown() {
        assert!(!EffectSummary::pure().conflicts_with(&EffectSummary::unknown()));
    }

    #[test]
    fn parses_effect_declarations() {
        let decl = EffectDeclaration::parse(Some("effects=pure,reads=project:src+host:/etc"));
        assert_eq!(decl.purity, Some(DeclaredPurity::Pure));
        assert!(decl.reads.contains(&ResourceKey::ProjectPath("src".into())));
        assert!(decl.reads.contains(&ResourceKey::HostPath("/etc".into())));
    }

    #[test]
    fn declared_pure_clears_unknown() {
        let decl = EffectDeclaration::parse(Some("effects=pure"));
        let summary = decl.apply(EffectSummary::unknown());
        assert!(!summary.unknown);
        assert!(summary.deterministic);
    }

    #[test]
    fn serial_host_forces_host_global_write() {
        let decl = EffectDeclaration::parse(Some("serial=host"));
        let summary = decl.apply(EffectSummary::pure());
        assert!(summary.writes.contains(&ResourceKey::HostGlobal));
    }
}
