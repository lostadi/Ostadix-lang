use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use super::digest::{validate_label, validate_token};
use super::{CanonicalPlacementRecordV1, PlacementValidationError, SemanticDigestV1};

/// Semantic capability coordinate.  The ISA name is intentionally not part of
/// the key; for example, `vector/reduce-width-agnostic` can be implemented by
/// both SVE and AVX-512 realizations.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityKeyV1 {
    namespace: String,
    name: String,
}

impl CapabilityKeyV1 {
    pub fn new(
        namespace: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<Self, PlacementValidationError> {
        let namespace = namespace.into();
        let name = name.into();
        validate_token("capability namespace", &namespace)?;
        validate_token("capability name", &name)?;
        Ok(Self { namespace, name })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for CapabilityKeyV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.namespace, self.name)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityKeyWireV1 {
    namespace: String,
    name: String,
}

impl<'de> Deserialize<'de> for CapabilityKeyV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CapabilityKeyWireV1::deserialize(deserializer)?;
        Self::new(wire.namespace, wire.name).map_err(serde::de::Error::custom)
    }
}

/// One point in a downward-closed semantic capability order.
///
/// Supporting level `n` means supporting every level `m <= n` for the same
/// key.  Levels begin at one; zero would create a second spelling for absence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityAtomV1 {
    key: CapabilityKeyV1,
    level: u32,
}

impl CapabilityAtomV1 {
    pub fn new(key: CapabilityKeyV1, level: u32) -> Result<Self, PlacementValidationError> {
        if level == 0 {
            return Err(PlacementValidationError::Zero {
                field: "capability level",
            });
        }
        Ok(Self { key, level })
    }

    pub fn key(&self) -> &CapabilityKeyV1 {
        &self.key
    }

    pub fn level(&self) -> u32 {
        self.level
    }
}

impl fmt::Display for CapabilityAtomV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.key, self.level)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityAtomWireV1 {
    key: CapabilityKeyV1,
    level: u32,
}

impl<'de> Deserialize<'de> for CapabilityAtomV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CapabilityAtomWireV1::deserialize(deserializer)?;
        Self::new(wire.key, wire.level).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EndiannessV1 {
    Little,
    Big,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum EnvironmentRequirementV1 {
    Stateless,
    Ephemeral,
    SameLogicalEnvironment { identity: SemanticDigestV1 },
    SameActorGeneration { identity: SemanticDigestV1 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectRequirementV1 {
    CompilerVerifiedPure,
    CompilerVerifiedIdempotent,
    AutonomousUnknownEffects,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceKindV1 {
    CpuSlots,
    MemoryBytes,
    ScratchBytes,
}

/// A single independently dischargeable placement requirement.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum RequirementAtomV1 {
    Capability(CapabilityAtomV1),
    BackendSpecification(SemanticDigestV1),
    BackendImplementation(SemanticDigestV1),
    OperatingSystem(String),
    Architecture(String),
    Abi(String),
    Endianness(EndiannessV1),
    MinimumPointerWidth(u16),
    PortableValueKind(String),
    Preservation(String),
    Environment(EnvironmentRequirementV1),
    Effect(EffectRequirementV1),
    ResourceMinimum {
        resource: ResourceKindV1,
        amount: u64,
    },
}

impl RequirementAtomV1 {
    pub fn operating_system(value: impl Into<String>) -> Result<Self, PlacementValidationError> {
        Self::validated_token("operating system", value, Self::OperatingSystem)
    }

    pub fn architecture(value: impl Into<String>) -> Result<Self, PlacementValidationError> {
        Self::validated_token("architecture", value, Self::Architecture)
    }

    pub fn abi(value: impl Into<String>) -> Result<Self, PlacementValidationError> {
        Self::validated_token("ABI", value, Self::Abi)
    }

    pub fn portable_value_kind(value: impl Into<String>) -> Result<Self, PlacementValidationError> {
        Self::validated_token("portable value kind", value, Self::PortableValueKind)
    }

    pub fn preservation(value: impl Into<String>) -> Result<Self, PlacementValidationError> {
        Self::validated_token("preservation property", value, Self::Preservation)
    }

    pub fn minimum_pointer_width(bits: u16) -> Result<Self, PlacementValidationError> {
        if bits == 0 || bits > 128 || bits % 8 != 0 {
            return Err(PlacementValidationError::InvalidToken {
                field: "minimum pointer width",
                value: bits.to_string(),
            });
        }
        Ok(Self::MinimumPointerWidth(bits))
    }

    pub fn resource_minimum(
        resource: ResourceKindV1,
        amount: u64,
    ) -> Result<Self, PlacementValidationError> {
        if amount == 0 {
            return Err(PlacementValidationError::Zero {
                field: "resource minimum",
            });
        }
        Ok(Self::ResourceMinimum { resource, amount })
    }

    fn validated_token(
        field: &'static str,
        value: impl Into<String>,
        wrap: impl FnOnce(String) -> Self,
    ) -> Result<Self, PlacementValidationError> {
        let value = value.into();
        validate_token(field, &value)?;
        Ok(wrap(value))
    }

    fn validate(&self) -> Result<(), PlacementValidationError> {
        match self {
            Self::OperatingSystem(value) => validate_token("operating system", value),
            Self::Architecture(value) => validate_token("architecture", value),
            Self::Abi(value) => validate_token("ABI", value),
            Self::PortableValueKind(value) => validate_token("portable value kind", value),
            Self::Preservation(value) => validate_token("preservation property", value),
            Self::MinimumPointerWidth(bits) => Self::minimum_pointer_width(*bits).map(|_| ()),
            Self::ResourceMinimum { resource, amount } => {
                Self::resource_minimum(*resource, *amount).map(|_| ())
            }
            Self::Capability(_)
            | Self::BackendSpecification(_)
            | Self::BackendImplementation(_)
            | Self::Endianness(_)
            | Self::Environment(_)
            | Self::Effect(_) => Ok(()),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Capability(atom) => format!("capability:{atom}"),
            Self::BackendSpecification(value) => format!("backend-spec:{value}"),
            Self::BackendImplementation(value) => format!("backend-implementation:{value}"),
            Self::OperatingSystem(value) => format!("os:{value}"),
            Self::Architecture(value) => format!("arch:{value}"),
            Self::Abi(value) => format!("abi:{value}"),
            Self::Endianness(value) => format!("endian:{value:?}"),
            Self::MinimumPointerWidth(value) => format!("pointer-width>={value}"),
            Self::PortableValueKind(value) => format!("portable-value:{value}"),
            Self::Preservation(value) => format!("preserves:{value}"),
            Self::Environment(value) => format!("environment:{value:?}"),
            Self::Effect(value) => format!("effect:{value:?}"),
            Self::ResourceMinimum { resource, amount } => {
                format!("resource:{resource:?}>={amount}")
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
enum RequirementAtomWireV1 {
    Capability(CapabilityAtomV1),
    BackendSpecification(SemanticDigestV1),
    BackendImplementation(SemanticDigestV1),
    OperatingSystem(String),
    Architecture(String),
    Abi(String),
    Endianness(EndiannessV1),
    MinimumPointerWidth(u16),
    PortableValueKind(String),
    Preservation(String),
    Environment(EnvironmentRequirementV1),
    Effect(EffectRequirementV1),
    ResourceMinimum {
        resource: ResourceKindV1,
        amount: u64,
    },
}

impl<'de> Deserialize<'de> for RequirementAtomV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RequirementAtomWireV1::deserialize(deserializer)?;
        let value = match wire {
            RequirementAtomWireV1::Capability(value) => Self::Capability(value),
            RequirementAtomWireV1::BackendSpecification(value) => Self::BackendSpecification(value),
            RequirementAtomWireV1::BackendImplementation(value) => {
                Self::BackendImplementation(value)
            }
            RequirementAtomWireV1::OperatingSystem(value) => Self::OperatingSystem(value),
            RequirementAtomWireV1::Architecture(value) => Self::Architecture(value),
            RequirementAtomWireV1::Abi(value) => Self::Abi(value),
            RequirementAtomWireV1::Endianness(value) => Self::Endianness(value),
            RequirementAtomWireV1::MinimumPointerWidth(value) => Self::MinimumPointerWidth(value),
            RequirementAtomWireV1::PortableValueKind(value) => Self::PortableValueKind(value),
            RequirementAtomWireV1::Preservation(value) => Self::Preservation(value),
            RequirementAtomWireV1::Environment(value) => Self::Environment(value),
            RequirementAtomWireV1::Effect(value) => Self::Effect(value),
            RequirementAtomWireV1::ResourceMinimum { resource, amount } => {
                Self::ResourceMinimum { resource, amount }
            }
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// A conservative, order-independent summary of a prepared operation.
///
/// `Complete(empty)` is the join identity.  Unknown information is represented
/// separately from absence of requirements and is absorbing over complete
/// footprints.  Unsatisfiable is the greatest state.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RequirementFootprintV1(RequirementFootprintStateV1);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "state")]
enum RequirementFootprintStateV1 {
    Complete {
        atoms: BTreeSet<RequirementAtomV1>,
    },
    ConservativeUnknown {
        known_atoms: BTreeSet<RequirementAtomV1>,
        reasons: BTreeSet<String>,
    },
    Unsatisfiable {
        known_atoms: BTreeSet<RequirementAtomV1>,
        reasons: BTreeSet<String>,
    },
}

impl RequirementFootprintV1 {
    pub fn complete(atoms: impl IntoIterator<Item = RequirementAtomV1>) -> Self {
        Self(RequirementFootprintStateV1::Complete {
            atoms: atoms.into_iter().collect(),
        })
    }

    pub fn empty() -> Self {
        Self::complete([])
    }

    pub fn conservative_unknown(
        known_atoms: impl IntoIterator<Item = RequirementAtomV1>,
        reasons: impl IntoIterator<Item = String>,
    ) -> Result<Self, PlacementValidationError> {
        let value = Self(RequirementFootprintStateV1::ConservativeUnknown {
            known_atoms: known_atoms.into_iter().collect(),
            reasons: reasons.into_iter().collect(),
        });
        value.validate()?;
        Ok(value)
    }

    pub fn unsatisfiable(
        known_atoms: impl IntoIterator<Item = RequirementAtomV1>,
        reasons: impl IntoIterator<Item = String>,
    ) -> Result<Self, PlacementValidationError> {
        let value = Self(RequirementFootprintStateV1::Unsatisfiable {
            known_atoms: known_atoms.into_iter().collect(),
            reasons: reasons.into_iter().collect(),
        });
        value.validate()?;
        Ok(value)
    }

    pub fn known_atoms(&self) -> &BTreeSet<RequirementAtomV1> {
        match &self.0 {
            RequirementFootprintStateV1::Complete { atoms } => atoms,
            RequirementFootprintStateV1::ConservativeUnknown { known_atoms, .. }
            | RequirementFootprintStateV1::Unsatisfiable { known_atoms, .. } => known_atoms,
        }
    }

    pub fn reasons(&self) -> Option<&BTreeSet<String>> {
        match &self.0 {
            RequirementFootprintStateV1::Complete { .. } => None,
            RequirementFootprintStateV1::ConservativeUnknown { reasons, .. }
            | RequirementFootprintStateV1::Unsatisfiable { reasons, .. } => Some(reasons),
        }
    }

    pub fn is_complete(&self) -> bool {
        matches!(self.0, RequirementFootprintStateV1::Complete { .. })
    }

    pub fn is_conservative_unknown(&self) -> bool {
        matches!(
            self.0,
            RequirementFootprintStateV1::ConservativeUnknown { .. }
        )
    }

    pub fn is_unsatisfiable(&self) -> bool {
        matches!(self.0, RequirementFootprintStateV1::Unsatisfiable { .. })
    }

    pub fn join(&self, other: &Self) -> Self {
        let known_atoms = self
            .known_atoms()
            .union(other.known_atoms())
            .cloned()
            .collect();
        let reasons = self
            .reasons()
            .into_iter()
            .flatten()
            .chain(other.reasons().into_iter().flatten())
            .cloned()
            .collect();
        if self.is_unsatisfiable() || other.is_unsatisfiable() {
            Self(RequirementFootprintStateV1::Unsatisfiable {
                known_atoms,
                reasons,
            })
        } else if self.is_conservative_unknown() || other.is_conservative_unknown() {
            Self(RequirementFootprintStateV1::ConservativeUnknown {
                known_atoms,
                reasons,
            })
        } else {
            Self(RequirementFootprintStateV1::Complete { atoms: known_atoms })
        }
    }

    pub fn require_complete(
        &self,
    ) -> Result<&BTreeSet<RequirementAtomV1>, PlacementValidationError> {
        match &self.0 {
            RequirementFootprintStateV1::Complete { atoms } => Ok(atoms),
            RequirementFootprintStateV1::ConservativeUnknown { reasons, .. } => Err(
                PlacementValidationError::ConservativeUnknown(reasons.iter().cloned().collect()),
            ),
            RequirementFootprintStateV1::Unsatisfiable { reasons, .. } => Err(
                PlacementValidationError::Unsatisfiable(reasons.iter().cloned().collect()),
            ),
        }
    }

    fn validate(&self) -> Result<(), PlacementValidationError> {
        for atom in self.known_atoms() {
            atom.validate()?;
        }
        if let Some(reasons) = self.reasons() {
            if reasons.is_empty() {
                return Err(PlacementValidationError::Empty {
                    field: "unknown/unsatisfiable reasons",
                });
            }
            for reason in reasons {
                validate_label("requirement reason", reason)?;
            }
        }
        Ok(())
    }
}

impl CanonicalPlacementRecordV1 for RequirementFootprintV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/requirement-footprint/v1";
}

impl Serialize for RequirementFootprintV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", tag = "state")]
enum RequirementFootprintWireV1 {
    Complete {
        atoms: BTreeSet<RequirementAtomV1>,
    },
    ConservativeUnknown {
        known_atoms: BTreeSet<RequirementAtomV1>,
        reasons: BTreeSet<String>,
    },
    Unsatisfiable {
        known_atoms: BTreeSet<RequirementAtomV1>,
        reasons: BTreeSet<String>,
    },
}

impl<'de> Deserialize<'de> for RequirementFootprintV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RequirementFootprintWireV1::deserialize(deserializer)?;
        let value = match wire {
            RequirementFootprintWireV1::Complete { atoms } => {
                Self(RequirementFootprintStateV1::Complete { atoms })
            }
            RequirementFootprintWireV1::ConservativeUnknown {
                known_atoms,
                reasons,
            } => Self(RequirementFootprintStateV1::ConservativeUnknown {
                known_atoms,
                reasons,
            }),
            RequirementFootprintWireV1::Unsatisfiable {
                known_atoms,
                reasons,
            } => Self(RequirementFootprintStateV1::Unsatisfiable {
                known_atoms,
                reasons,
            }),
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}
