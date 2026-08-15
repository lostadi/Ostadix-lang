use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};

use crate::world::ArtifactId;

use super::digest::{validate_label, validate_token};
use super::{
    CanonicalPlacementRecordV1, CapabilityAtomV1, CapabilityKeyV1, CurrentBackendCatalogV1,
    EndiannessV1, PlacementValidationError, RequirementAtomV1, RequirementFootprintV1,
    SemanticDigestV1,
};

/// Placement V1 deliberately accepts only capability ideals. Non-downward-closed
/// accelerator capability relations require a different solver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetCapabilityModelV1 {
    DownwardClosedIdeal,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformDescriptorV1 {
    operating_system: String,
    architecture: String,
    abi: String,
    endianness: EndiannessV1,
    pointer_width: u16,
}

impl PlatformDescriptorV1 {
    pub fn new(
        operating_system: impl Into<String>,
        architecture: impl Into<String>,
        abi: impl Into<String>,
        endianness: EndiannessV1,
        pointer_width: u16,
    ) -> Result<Self, PlacementValidationError> {
        let operating_system = operating_system.into();
        let architecture = architecture.into();
        let abi = abi.into();
        validate_token("target operating system", &operating_system)?;
        validate_token("target architecture", &architecture)?;
        validate_token("target ABI", &abi)?;
        if pointer_width == 0 || pointer_width > 128 || !pointer_width.is_multiple_of(8) {
            return Err(PlacementValidationError::InvalidToken {
                field: "target pointer width",
                value: pointer_width.to_string(),
            });
        }
        Ok(Self {
            operating_system,
            architecture,
            abi,
            endianness,
            pointer_width,
        })
    }

    pub fn operating_system(&self) -> &str {
        &self.operating_system
    }

    pub fn architecture(&self) -> &str {
        &self.architecture
    }

    pub fn abi(&self) -> &str {
        &self.abi
    }

    pub fn endianness(&self) -> EndiannessV1 {
        self.endianness
    }

    pub fn pointer_width(&self) -> u16 {
        self.pointer_width
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformDescriptorWireV1 {
    operating_system: String,
    architecture: String,
    abi: String,
    endianness: EndiannessV1,
    pointer_width: u16,
}

impl<'de> Deserialize<'de> for PlatformDescriptorV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PlatformDescriptorWireV1::deserialize(deserializer)?;
        Self::new(
            wire.operating_system,
            wire.architecture,
            wire.abi,
            wire.endianness,
            wire.pointer_width,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Exact realization pipeline identity for one backend implementation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendImplementationIdV1 {
    backend_specification: SemanticDigestV1,
    adapter_artifact: ArtifactId,
    executable_set: SemanticDigestV1,
    protocol_abi: String,
    realization_pipeline: SemanticDigestV1,
}

impl BackendImplementationIdV1 {
    pub fn new(
        backend_specification: SemanticDigestV1,
        adapter_artifact: ArtifactId,
        executable_set: SemanticDigestV1,
        protocol_abi: impl Into<String>,
        realization_pipeline: SemanticDigestV1,
    ) -> Result<Self, PlacementValidationError> {
        let protocol_abi = protocol_abi.into();
        validate_token("backend protocol ABI", &protocol_abi)?;
        Ok(Self {
            backend_specification,
            adapter_artifact,
            executable_set,
            protocol_abi,
            realization_pipeline,
        })
    }

    pub fn backend_specification(&self) -> &SemanticDigestV1 {
        &self.backend_specification
    }

    pub fn adapter_artifact(&self) -> &ArtifactId {
        &self.adapter_artifact
    }

    pub fn executable_set(&self) -> &SemanticDigestV1 {
        &self.executable_set
    }

    pub fn protocol_abi(&self) -> &str {
        &self.protocol_abi
    }

    pub fn realization_pipeline(&self) -> &SemanticDigestV1 {
        &self.realization_pipeline
    }
}

impl CanonicalPlacementRecordV1 for BackendImplementationIdV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/backend-implementation/v1";
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendImplementationWireV1 {
    backend_specification: SemanticDigestV1,
    adapter_artifact: ArtifactId,
    executable_set: SemanticDigestV1,
    protocol_abi: String,
    realization_pipeline: SemanticDigestV1,
}

impl<'de> Deserialize<'de> for BackendImplementationIdV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BackendImplementationWireV1::deserialize(deserializer)?;
        Self::new(
            wire.backend_specification,
            wire.adapter_artifact,
            wire.executable_set,
            wire.protocol_abi,
            wire.realization_pipeline,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Physical evaluator identity.  Logical environment serialization must use a
/// separate logical identity; a generation change never erases state affinity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorGenerationIdV1 {
    logical_environment: SemanticDigestV1,
    backend_implementation: SemanticDigestV1,
    target_descriptor: SemanticDigestV1,
    sandbox_policy: SemanticDigestV1,
    launch_context: SemanticDigestV1,
    generation: super::GenerationV1,
}

impl ActorGenerationIdV1 {
    pub fn new(
        logical_environment: SemanticDigestV1,
        backend_implementation: SemanticDigestV1,
        target_descriptor: SemanticDigestV1,
        sandbox_policy: SemanticDigestV1,
        launch_context: SemanticDigestV1,
        generation: super::GenerationV1,
    ) -> Self {
        Self {
            logical_environment,
            backend_implementation,
            target_descriptor,
            sandbox_policy,
            launch_context,
            generation,
        }
    }

    pub fn logical_environment(&self) -> &SemanticDigestV1 {
        &self.logical_environment
    }

    pub fn backend_implementation(&self) -> &SemanticDigestV1 {
        &self.backend_implementation
    }

    pub fn target_descriptor(&self) -> &SemanticDigestV1 {
        &self.target_descriptor
    }

    pub fn sandbox_policy(&self) -> &SemanticDigestV1 {
        &self.sandbox_policy
    }

    pub fn launch_context(&self) -> &SemanticDigestV1 {
        &self.launch_context
    }

    pub fn generation(&self) -> super::GenerationV1 {
        self.generation
    }
}

impl CanonicalPlacementRecordV1 for ActorGenerationIdV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/actor-generation/v1";
}

/// Stable target facts.  Capacity is deliberately absent and changes on a
/// separate observation generation.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetDescriptorV1 {
    node_id: String,
    display_name: String,
    node_generation: super::GenerationV1,
    capability_model: TargetCapabilityModelV1,
    platform: PlatformDescriptorV1,
    capabilities: BTreeSet<CapabilityAtomV1>,
    raw_cpu_features: BTreeSet<String>,
    backend_implementations: BTreeSet<BackendImplementationIdV1>,
}

impl TargetDescriptorV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: impl Into<String>,
        display_name: impl Into<String>,
        node_generation: super::GenerationV1,
        capability_model: TargetCapabilityModelV1,
        platform: PlatformDescriptorV1,
        capabilities: impl IntoIterator<Item = CapabilityAtomV1>,
        raw_cpu_features: impl IntoIterator<Item = String>,
        backend_implementations: impl IntoIterator<Item = BackendImplementationIdV1>,
    ) -> Result<Self, PlacementValidationError> {
        if capability_model != TargetCapabilityModelV1::DownwardClosedIdeal {
            return Err(PlacementValidationError::UnsupportedCapabilityModel);
        }
        let node_id = node_id.into();
        let display_name = display_name.into();
        validate_token("placement node identity", &node_id)?;
        validate_label("placement node display name", &display_name)?;
        let mut capability_levels = BTreeMap::<CapabilityKeyV1, u32>::new();
        for capability in capabilities {
            capability_levels
                .entry(capability.key().clone())
                .and_modify(|level| *level = (*level).max(capability.level()))
                .or_insert(capability.level());
        }
        let raw_cpu_features: BTreeSet<String> = raw_cpu_features.into_iter().collect();
        for feature in &raw_cpu_features {
            validate_token("raw CPU feature", feature)?;
        }
        let capabilities = capability_levels
            .into_iter()
            .map(|(key, level)| CapabilityAtomV1::new(key, level))
            .collect::<Result<BTreeSet<_>, _>>()?;
        Ok(Self {
            node_id,
            display_name,
            node_generation,
            capability_model,
            platform,
            capabilities,
            raw_cpu_features,
            backend_implementations: backend_implementations.into_iter().collect(),
        })
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn node_generation(&self) -> super::GenerationV1 {
        self.node_generation
    }

    pub fn platform(&self) -> &PlatformDescriptorV1 {
        &self.platform
    }

    pub fn capabilities(&self) -> &BTreeSet<CapabilityAtomV1> {
        &self.capabilities
    }

    pub fn backend_implementations(&self) -> &BTreeSet<BackendImplementationIdV1> {
        &self.backend_implementations
    }

    /// Reject backend identities minted under an older catalog or realization
    /// hash domain.
    ///
    /// The records themselves remain decodable and their detached signatures
    /// remain independently inspectable.  They cannot, however, authorize a
    /// placement against this process unless every advertised backend
    /// specification and complete realization belong to the current compiled
    /// catalog. Because the catalog schema and realization formula are both
    /// checked, either rollover fails closed without rewriting the signed
    /// target-record schema.
    pub fn validate_current_backend_catalog_with(
        &self,
        catalog: &impl CurrentBackendCatalogV1,
    ) -> Result<(), PlacementValidationError> {
        for implementation in &self.backend_implementations {
            let specification = implementation.backend_specification();
            if !catalog.contains_current_specification(specification) {
                return Err(PlacementValidationError::NonCurrentBackendCatalog {
                    specification: specification.as_sha256().to_owned(),
                    current_schema: catalog.current_schema().to_owned(),
                });
            }
            if !catalog.contains_current_implementation(implementation) {
                return Err(PlacementValidationError::NonCurrentBackendImplementation {
                    realization_pipeline: implementation
                        .realization_pipeline()
                        .as_sha256()
                        .to_owned(),
                    current_schema: catalog.current_schema().to_owned(),
                });
            }
        }
        Ok(())
    }

    pub fn supports_capability(&self, atom: &CapabilityAtomV1) -> bool {
        self.capabilities
            .iter()
            .any(|supported| supported.key() == atom.key() && supported.level() >= atom.level())
    }

    pub fn supports_requirement(
        &self,
        requirement: &RequirementAtomV1,
    ) -> Result<bool, PlacementValidationError> {
        Ok(match requirement {
            RequirementAtomV1::Capability(atom) => self.supports_capability(atom),
            RequirementAtomV1::BackendSpecification(expected) => self
                .backend_implementations
                .iter()
                .any(|implementation| implementation.backend_specification() == expected),
            RequirementAtomV1::BackendImplementation(expected) => self
                .backend_implementations
                .iter()
                .filter_map(|implementation| implementation.semantic_digest().ok())
                .any(|actual| &actual == expected),
            RequirementAtomV1::OperatingSystem(expected) => {
                self.platform.operating_system() == expected
            }
            RequirementAtomV1::Architecture(expected) => self.platform.architecture() == expected,
            RequirementAtomV1::Abi(expected) => self.platform.abi() == expected,
            RequirementAtomV1::Endianness(expected) => self.platform.endianness() == *expected,
            RequirementAtomV1::MinimumPointerWidth(expected) => {
                self.platform.pointer_width() >= *expected
            }
            RequirementAtomV1::PortableValueKind(kind) => {
                self.supports_named("portable-value", kind)
            }
            RequirementAtomV1::Preservation(property) => {
                self.supports_named("preservation", property)
            }
            RequirementAtomV1::Environment(_)
            | RequirementAtomV1::Effect(_)
            | RequirementAtomV1::ResourceMinimum { .. } => true,
        })
    }

    fn supports_named(&self, namespace: &str, name: &str) -> bool {
        CapabilityKeyV1::new(namespace, name)
            .ok()
            .is_some_and(|key| {
                self.capabilities
                    .iter()
                    .any(|capability| capability.key() == &key && capability.level() >= 1)
            })
    }

    pub fn codegen_projection_digest(
        &self,
        footprint: &RequirementFootprintV1,
        backend: &BackendImplementationIdV1,
    ) -> Result<SemanticDigestV1, PlacementValidationError> {
        let atoms = footprint.require_complete()?;
        let relevant_capabilities: BTreeSet<_> = atoms
            .iter()
            .filter_map(|atom| match atom {
                RequirementAtomV1::Capability(capability) => self
                    .capabilities
                    .iter()
                    .find(|supported| supported.key() == capability.key())
                    .cloned(),
                _ => None,
            })
            .collect();
        CodegenProjectionV1 {
            platform: self.platform.clone(),
            relevant_capabilities,
            backend_implementation: backend.semantic_digest()?,
        }
        .semantic_digest()
    }
}

impl CanonicalPlacementRecordV1 for TargetDescriptorV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/target-descriptor/v1";
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetDescriptorWireV1 {
    node_id: String,
    display_name: String,
    node_generation: super::GenerationV1,
    capability_model: TargetCapabilityModelV1,
    platform: PlatformDescriptorV1,
    capabilities: BTreeSet<CapabilityAtomV1>,
    raw_cpu_features: BTreeSet<String>,
    backend_implementations: BTreeSet<BackendImplementationIdV1>,
}

impl<'de> Deserialize<'de> for TargetDescriptorV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TargetDescriptorWireV1::deserialize(deserializer)?;
        Self::new(
            wire.node_id,
            wire.display_name,
            wire.node_generation,
            wire.capability_model,
            wire.platform,
            wire.capabilities,
            wire.raw_cpu_features,
            wire.backend_implementations,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
struct CodegenProjectionV1 {
    platform: PlatformDescriptorV1,
    relevant_capabilities: BTreeSet<CapabilityAtomV1>,
    backend_implementation: SemanticDigestV1,
}

impl CanonicalPlacementRecordV1 for CodegenProjectionV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/codegen-projection/v1";
}

/// Artifact cache identity.  Node names, ISA display names, profile generation,
/// and dynamic capacity are absent by construction.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCacheKeyV1 {
    operation_oir: ArtifactId,
    compiler: SemanticDigestV1,
    analyzer: SemanticDigestV1,
    backend_implementation: SemanticDigestV1,
    target_codegen_projection: SemanticDigestV1,
    optimization_policy: SemanticDigestV1,
}

impl ArtifactCacheKeyV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operation_oir: ArtifactId,
        compiler: SemanticDigestV1,
        analyzer: SemanticDigestV1,
        backend: &BackendImplementationIdV1,
        target: &TargetDescriptorV1,
        footprint: &RequirementFootprintV1,
        optimization_policy: SemanticDigestV1,
    ) -> Result<Self, PlacementValidationError> {
        Ok(Self {
            operation_oir,
            compiler,
            analyzer,
            backend_implementation: backend.semantic_digest()?,
            target_codegen_projection: target.codegen_projection_digest(footprint, backend)?,
            optimization_policy,
        })
    }
}

impl CanonicalPlacementRecordV1 for ArtifactCacheKeyV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/artifact-cache-key/v1";
}
