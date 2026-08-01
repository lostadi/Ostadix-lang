//! Shared semantic contract for source-integrated and binary-contained kernels.
//!
//! This module is deliberately a host-side reference surface. It validates a
//! strict, bounded `ocore.kernel-world/v1` manifest and models the generation,
//! health, request, failure, replacement, export, and provenance rules that any
//! future O-core implementation must preserve. It does not create a VM, boot a
//! foreign kernel, assign a device, or provide DMA isolation.

use crate::live_system::manifest::{runtime_entry_payload_path, PackageDigest, VerifiedPackage};
use crate::world::identity::{DomainIdentity, KernelWorldBinding, WorldIdentityError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use thiserror::Error;

/// Strict manifest schema shared by both kernel-world integration tracks.
pub const KERNEL_WORLD_SCHEMA_V1: &str = "ocore.kernel-world/v1";
/// Versioned control-plane protocol expected from a kernel-world provider.
pub const KERNEL_WORLD_CONTROL_PROTOCOL_V1: &str = "ocore.kernel-world-control/v1";
/// `ocore.package/v1` runtime kind used to bind a world to verified bytes.
pub const KERNEL_WORLD_RUNTIME_KIND: &str = "kernel-world";

/// Compact normal-form record accepted by the first native O-core admission
/// slice. The record is an output of `VerifiedKernelWorld`; decoding bytes is
/// inspection only and never recreates package-verification authority.
pub const NATIVE_KERNEL_WORLD_RECORD_V1: u16 = 1;
pub const NATIVE_KERNEL_WORLD_RECORD_V2: u16 = 2;
pub const MAX_NATIVE_KERNEL_WORLD_RECORD_BYTES: usize = 16 * 1024;
pub const MAX_NATIVE_KERNEL_WORLD_EXPORTS: usize = 4;
pub const MAX_NATIVE_KERNEL_WORLD_CAPABILITY_REQUESTS: usize = 8;
const NATIVE_KERNEL_WORLD_MAGIC: &[u8; 8] = b"OKWORLD1";

pub const MAX_KERNEL_WORLD_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_KERNEL_WORLD_EXPORTS: usize = 64;
pub const MAX_KERNEL_WORLD_CAPABILITY_REQUESTS: usize = 64;
pub const MAX_KERNEL_WORLD_RIGHTS: usize = 16;
pub const MAX_KERNEL_WORLD_REQUIREMENTS: usize = 32;
pub const MAX_KERNEL_WORLD_IDENTIFIER_BYTES: usize = 128;
pub const MAX_KERNEL_WORLD_TEXT_BYTES: usize = 512;
pub const MAX_KERNEL_WORLD_VCPUS: u16 = 256;
pub const MAX_KERNEL_WORLD_MEMORY_MIB: u64 = 1024 * 1024;
pub const MAX_KERNEL_WORLD_OUTSTANDING_REQUESTS: u32 = 65_536;
pub const MAX_KERNEL_WORLD_REQUESTS_PER_GENERATION: u64 = 1_000_000;
pub const MAX_KERNEL_WORLD_SHARED_MEMORY_BYTES: u64 = 1 << 40;
pub const MAX_KERNEL_WORLD_DEVICES: u16 = 256;

/// One installable kernel world, independent of how that world is executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelWorldManifest {
    pub schema: String,
    pub name: String,
    pub version: String,
    pub integration: IntegrationMode,
    pub image: KernelImage,
    pub machine: MachineContract,
    pub lifecycle: LifecycleContract,
    pub quotas: ResourceQuotas,
    pub exports: Vec<WorldExport>,
    pub capability_requests: Vec<WorldCapabilityRequest>,
    pub license: LicenseContract,
}

/// The two implementation families converge on the same public contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationMode {
    /// A source-available kernel port using O-core's paravirtual contract.
    SourceIntegrated,
    /// A kernel image contained behind hardware virtualization.
    BinaryContained,
}

/// The immutable image reference. User-supplied images remain hash-pinned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum KernelImage {
    PackagePayload { path: String, sha256: String },
    UserSupplied { expected_sha256: String },
}

/// Machine assumptions required to instantiate the image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContract {
    pub guest_architecture: String,
    pub profile: String,
    pub execution: ExecutionMechanism,
    pub firmware: FirmwareContract,
    pub min_vcpus: u16,
    pub max_vcpus: u16,
    pub min_memory_mib: u64,
    pub max_memory_mib: u64,
    pub requirements: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMechanism {
    Paravirtual,
    HardwareVirtualized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirmwareContract {
    Direct,
    Uefi,
}

/// Health and replacement policy; it never grants authority to restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleContract {
    pub health_protocol: String,
    pub health_timeout_ms: u64,
    pub restart: RestartPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    Never,
    OnFailure,
    Always,
}

/// Hard admission and runtime ceilings for one generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceQuotas {
    pub max_outstanding_requests: u32,
    pub max_requests_per_generation: u64,
    pub max_shared_memory_bytes: u64,
    pub max_devices: u16,
}

/// An exported interface. Clients bind to this contract, not provider internals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldExport {
    pub name: String,
    pub plane: ExportPlane,
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_request: Option<String>,
}

/// Foreign kernels may import hardware, ABI, or higher-level semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportPlane {
    Device,
    Abi,
    Semantic,
}

/// Requested authority. A manifest describes a request; policy grants it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldCapabilityRequest {
    pub kind: String,
    pub rights: Vec<String>,
    pub purpose: String,
}

/// Deployment metadata only. This is not a legal determination or authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LicenseContract {
    pub redistribution: RedistributionPolicy,
    pub external_acceptance_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedistributionPolicy {
    Redistributable,
    UserSuppliedOnly,
}

#[derive(Debug, Error)]
pub enum KernelWorldError {
    #[error("invalid kernel-world manifest TOML: {0}")]
    InvalidToml(#[from] toml::de::Error),

    #[error("could not encode canonical kernel-world manifest TOML: {0}")]
    EncodeToml(#[from] toml::ser::Error),

    #[error("unsupported kernel-world schema `{found}`; expected `{KERNEL_WORLD_SCHEMA_V1}`")]
    UnsupportedSchema { found: String },

    #[error("invalid kernel-world field `{field}`: {reason}")]
    InvalidField { field: &'static str, reason: String },

    #[error("{resource} exceeds its limit of {limit} (got {actual})")]
    LimitExceeded {
        resource: &'static str,
        limit: u64,
        actual: u64,
    },

    #[error("duplicate value `{value}` in kernel-world field `{field}`")]
    Duplicate { field: &'static str, value: String },

    #[error("kernel-world package binding failed: {reason}")]
    PackageBinding { reason: String },

    #[error("cannot {action} while kernel world is {state:?}")]
    InvalidState {
        action: &'static str,
        state: InstanceState,
    },

    #[error("stale kernel-world generation {got}; current generation is {expected}")]
    StaleGeneration { expected: u64, got: u64 },

    #[error("kernel-world generation counter is exhausted")]
    GenerationExhausted,

    #[error("kernel-world request sequence is exhausted for generation {generation}")]
    RequestSequenceExhausted { generation: u64 },

    #[error("kernel world does not export `{name}`")]
    UnknownExport { name: String },

    #[error("kernel-world outstanding-request quota is exhausted")]
    OutstandingRequestLimit,

    #[error("kernel-world per-generation request quota is exhausted")]
    GenerationRequestLimit,

    #[error("unknown kernel-world request {request}")]
    UnknownRequest { request: RequestId },

    #[error("kernel-world request {request} is already terminal as {terminal:?}")]
    RequestAlreadyTerminal {
        request: RequestId,
        terminal: RequestTerminal,
    },

    #[error("invalid native kernel-world record: {reason}")]
    InvalidNativeRecord { reason: String },
}

/// A deterministic native-admission record tied to one verified package.
///
/// The external SHA-256 of `bytes()` is the object identity that native
/// admission must pin. Only `VerifiedKernelWorld::encode_native_record` can
/// construct this authority-bearing type. Untrusted bytes decode to the
/// deliberately distinct `InspectedNativeKernelWorldRecord` type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeKernelWorldRecord {
    data: NativeKernelWorldRecordData,
}

/// Canonical descriptive data decoded from an untrusted native record.
///
/// Successful inspection proves that the bytes are structurally valid and in
/// canonical normal form. It does not prove that the referenced package was
/// verified, and this type therefore cannot be substituted for a
/// `NativeKernelWorldRecord` produced from `VerifiedKernelWorld`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedNativeKernelWorldRecord {
    data: NativeKernelWorldRecordData,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeKernelWorldRecordData {
    bytes: Vec<u8>,
    package_digest: PackageDigest,
    manifest: KernelWorldManifest,
}

impl KernelWorldManifest {
    /// Parse strict TOML and apply all cross-field admission rules.
    pub fn parse_toml(input: &str) -> Result<Self, KernelWorldError> {
        enforce_limit(
            "kernel-world manifest bytes",
            input.len() as u64,
            MAX_KERNEL_WORLD_MANIFEST_BYTES as u64,
        )?;
        let manifest: Self = toml::from_str(input)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate identifiers, bounds, duplicates, and integration-mode rules.
    pub fn validate(&self) -> Result<(), KernelWorldError> {
        if self.schema != KERNEL_WORLD_SCHEMA_V1 {
            return Err(KernelWorldError::UnsupportedSchema {
                found: self.schema.clone(),
            });
        }
        validate_name("name", &self.name)?;
        semver::Version::parse(&self.version).map_err(|error| KernelWorldError::InvalidField {
            field: "version",
            reason: error.to_string(),
        })?;

        match &self.image {
            KernelImage::PackagePayload { path, sha256 } => {
                validate_payload_path("image.path", path)?;
                validate_sha256("image.sha256", sha256)?;
            }
            KernelImage::UserSupplied { expected_sha256 } => {
                validate_sha256("image.expected_sha256", expected_sha256)?;
                if self.license.redistribution != RedistributionPolicy::UserSuppliedOnly {
                    return Err(KernelWorldError::InvalidField {
                        field: "license.redistribution",
                        reason: "a user-supplied image must use `user_supplied_only`".into(),
                    });
                }
                if !self.license.external_acceptance_required {
                    return Err(KernelWorldError::InvalidField {
                        field: "license.external_acceptance_required",
                        reason: "a user-supplied image must require external acceptance".into(),
                    });
                }
            }
        }

        validate_identifier(
            "machine.guest_architecture",
            &self.machine.guest_architecture,
        )?;
        validate_protocol("machine.profile", &self.machine.profile)?;
        validate_range_u16(
            "machine.vcpus",
            self.machine.min_vcpus,
            self.machine.max_vcpus,
            MAX_KERNEL_WORLD_VCPUS,
        )?;
        validate_range_u64(
            "machine.memory_mib",
            self.machine.min_memory_mib,
            self.machine.max_memory_mib,
            MAX_KERNEL_WORLD_MEMORY_MIB,
        )?;
        enforce_limit(
            "machine requirements",
            self.machine.requirements.len() as u64,
            MAX_KERNEL_WORLD_REQUIREMENTS as u64,
        )?;
        let mut requirements = BTreeSet::new();
        for requirement in &self.machine.requirements {
            validate_identifier("machine.requirements", requirement)?;
            if !requirements.insert(requirement) {
                return Err(KernelWorldError::Duplicate {
                    field: "machine.requirements",
                    value: requirement.clone(),
                });
            }
        }

        match self.integration {
            IntegrationMode::SourceIntegrated => {
                if !matches!(self.image, KernelImage::PackagePayload { .. }) {
                    return Err(KernelWorldError::InvalidField {
                        field: "image.kind",
                        reason: "a source-integrated world must use a package payload".into(),
                    });
                }
                if self.machine.execution != ExecutionMechanism::Paravirtual {
                    return Err(KernelWorldError::InvalidField {
                        field: "machine.execution",
                        reason: "a source-integrated world must use `paravirtual`".into(),
                    });
                }
                if self.machine.firmware != FirmwareContract::Direct {
                    return Err(KernelWorldError::InvalidField {
                        field: "machine.firmware",
                        reason: "a source-integrated world must use `direct` entry".into(),
                    });
                }
            }
            IntegrationMode::BinaryContained => {
                if self.machine.execution != ExecutionMechanism::HardwareVirtualized {
                    return Err(KernelWorldError::InvalidField {
                        field: "machine.execution",
                        reason: "a binary-contained world must use `hardware_virtualized`".into(),
                    });
                }
                if !self.capability_requests.iter().any(|request| {
                    request.kind == "vm.machine"
                        && request.rights.iter().any(|right| right == "run")
                }) {
                    return Err(KernelWorldError::InvalidField {
                        field: "capability_requests",
                        reason:
                            "a binary-contained world must request `vm.machine` `run` authority"
                                .into(),
                    });
                }
            }
        }

        validate_protocol("lifecycle.health_protocol", &self.lifecycle.health_protocol)?;
        if !(1..=60_000).contains(&self.lifecycle.health_timeout_ms) {
            return Err(KernelWorldError::InvalidField {
                field: "lifecycle.health_timeout_ms",
                reason: "must be between 1 and 60000 milliseconds".into(),
            });
        }

        validate_nonzero_limit(
            "quotas.max_outstanding_requests",
            self.quotas.max_outstanding_requests as u64,
            MAX_KERNEL_WORLD_OUTSTANDING_REQUESTS as u64,
        )?;
        validate_nonzero_limit(
            "quotas.max_requests_per_generation",
            self.quotas.max_requests_per_generation,
            MAX_KERNEL_WORLD_REQUESTS_PER_GENERATION,
        )?;
        if self.quotas.max_requests_per_generation < self.quotas.max_outstanding_requests as u64 {
            return Err(KernelWorldError::InvalidField {
                field: "quotas.max_requests_per_generation",
                reason: "must be at least `max_outstanding_requests`".into(),
            });
        }
        enforce_limit(
            "quotas.max_shared_memory_bytes",
            self.quotas.max_shared_memory_bytes,
            MAX_KERNEL_WORLD_SHARED_MEMORY_BYTES,
        )?;
        enforce_limit(
            "quotas.max_devices",
            self.quotas.max_devices as u64,
            MAX_KERNEL_WORLD_DEVICES as u64,
        )?;

        if self.exports.is_empty() {
            return Err(KernelWorldError::InvalidField {
                field: "exports",
                reason: "must declare at least one exported contract".into(),
            });
        }
        enforce_limit(
            "kernel-world exports",
            self.exports.len() as u64,
            MAX_KERNEL_WORLD_EXPORTS as u64,
        )?;
        let mut exports = BTreeSet::new();
        for export in &self.exports {
            validate_identifier("exports.name", &export.name)?;
            validate_protocol("exports.protocol", &export.protocol)?;
            if !exports.insert(export.name.as_str()) {
                return Err(KernelWorldError::Duplicate {
                    field: "exports.name",
                    value: export.name.clone(),
                });
            }
        }

        if self.capability_requests.is_empty() {
            return Err(KernelWorldError::InvalidField {
                field: "capability_requests",
                reason: "kernel worlds must declare required authority explicitly".into(),
            });
        }
        enforce_limit(
            "kernel-world capability requests",
            self.capability_requests.len() as u64,
            MAX_KERNEL_WORLD_CAPABILITY_REQUESTS as u64,
        )?;
        let mut requests = BTreeSet::new();
        for request in &self.capability_requests {
            validate_identifier("capability_requests.kind", &request.kind)?;
            if request.kind == "device." {
                return Err(KernelWorldError::InvalidField {
                    field: "capability_requests.kind",
                    reason: "a `device.*` request kind must include a non-empty suffix".into(),
                });
            }
            validate_text("capability_requests.purpose", &request.purpose)?;
            if request.rights.is_empty() {
                return Err(KernelWorldError::InvalidField {
                    field: "capability_requests.rights",
                    reason: "must contain at least one right".into(),
                });
            }
            enforce_limit(
                "rights per kernel-world capability request",
                request.rights.len() as u64,
                MAX_KERNEL_WORLD_RIGHTS as u64,
            )?;
            let mut rights = BTreeSet::new();
            for right in &request.rights {
                validate_identifier("capability_requests.rights", right)?;
                let is_reserved = matches!(right.as_str(), "run" | "stop" | "reset" | "dma");
                let permitted = if request.kind == "vm.machine" {
                    matches!(right.as_str(), "run" | "stop")
                } else if is_device_capability_kind(&request.kind) {
                    matches!(right.as_str(), "reset" | "dma")
                } else {
                    !is_reserved
                };
                if !permitted {
                    return Err(KernelWorldError::InvalidField {
                        field: "capability_requests.rights",
                        reason: format!(
                            "request kind `{}` may not use right `{right}`",
                            request.kind
                        ),
                    });
                }
                if !rights.insert(right.as_str()) {
                    return Err(KernelWorldError::Duplicate {
                        field: "capability_requests.rights",
                        value: right.clone(),
                    });
                }
            }
            if !requests.insert(request.kind.as_str()) {
                return Err(KernelWorldError::Duplicate {
                    field: "capability_requests.kind",
                    value: request.kind.clone(),
                });
            }
        }

        let mut device_authorities = BTreeSet::new();
        for export in &self.exports {
            match (export.plane, export.authority_request.as_deref()) {
                (ExportPlane::Device, None) => {
                    return Err(KernelWorldError::InvalidField {
                        field: "exports.authority_request",
                        reason: format!(
                            "device-plane export `{}` must name an exact `device.*` capability request",
                            export.name
                        ),
                    });
                }
                (ExportPlane::Device, Some(authority_request)) => {
                    validate_identifier("exports.authority_request", authority_request)?;
                    if !is_device_capability_kind(authority_request)
                        || !requests.contains(authority_request)
                    {
                        return Err(KernelWorldError::InvalidField {
                            field: "exports.authority_request",
                            reason: format!(
                                "device-plane export `{}` must name an exact existing `device.*` capability request; got `{authority_request}`",
                                export.name
                            ),
                        });
                    }
                    device_authorities.insert(authority_request);
                }
                (_, Some(authority_request)) => {
                    return Err(KernelWorldError::InvalidField {
                        field: "exports.authority_request",
                        reason: format!(
                            "non-device export `{}` must omit authority request `{authority_request}`",
                            export.name
                        ),
                    });
                }
                (_, None) => {}
            }
        }
        enforce_limit(
            "distinct device authority requests",
            device_authorities.len() as u64,
            self.quotas.max_devices as u64,
        )?;
        Ok(())
    }

    /// Stable human-readable encoding. Declaration order is non-semantic.
    pub fn canonical_toml(&self) -> Result<String, KernelWorldError> {
        self.validate()?;
        let mut manifest = self.clone();
        manifest.machine.requirements.sort();
        manifest.exports.sort_by(|left, right| {
            (
                &left.name,
                left.plane,
                &left.protocol,
                &left.authority_request,
            )
                .cmp(&(
                    &right.name,
                    right.plane,
                    &right.protocol,
                    &right.authority_request,
                ))
        });
        for request in &mut manifest.capability_requests {
            request.rights.sort();
        }
        manifest.capability_requests.sort_by(|left, right| {
            (&left.kind, &left.purpose, &left.rights).cmp(&(
                &right.kind,
                &right.purpose,
                &right.rights,
            ))
        });
        let output = toml::to_string(&manifest)?;
        enforce_limit(
            "canonical kernel-world manifest bytes",
            output.len() as u64,
            MAX_KERNEL_WORLD_MANIFEST_BYTES as u64,
        )?;
        Ok(output)
    }
}

/// A strict world manifest bound to one verified immutable package object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedKernelWorld {
    manifest: KernelWorldManifest,
    package_digest: PackageDigest,
}

impl VerifiedKernelWorld {
    /// Load the manifest from the package runtime entry and require the outer
    /// package metadata to describe exactly the same world boundary.
    pub fn from_package(package: &VerifiedPackage) -> Result<Self, KernelWorldError> {
        let package_manifest = package.manifest();
        if package_manifest.runtime.kind != KERNEL_WORLD_RUNTIME_KIND {
            return package_binding(format!(
                "runtime kind is `{}`; expected `{KERNEL_WORLD_RUNTIME_KIND}`",
                package_manifest.runtime.kind
            ));
        }
        if package_manifest.runtime.abi != KERNEL_WORLD_CONTROL_PROTOCOL_V1 {
            return package_binding(format!(
                "runtime ABI is `{}`; expected `{KERNEL_WORLD_CONTROL_PROTOCOL_V1}`",
                package_manifest.runtime.abi
            ));
        }

        let entry =
            runtime_entry_payload_path(&package_manifest.runtime.entry).map_err(|error| {
                KernelWorldError::PackageBinding {
                    reason: error.to_string(),
                }
            })?;
        let entry = entry
            .to_str()
            .ok_or_else(|| KernelWorldError::PackageBinding {
                reason: "runtime entry is not valid UTF-8".into(),
            })?;
        let manifest_file = package
            .payload_files()
            .iter()
            .find(|file| file.path() == entry)
            .ok_or_else(|| KernelWorldError::PackageBinding {
                reason: format!(
                    "runtime entry `{}` is absent from the payload",
                    package_manifest.runtime.entry
                ),
            })?;
        if manifest_file.is_executable() {
            return package_binding("kernel-world manifest entry must not be executable".into());
        }
        let manifest_text = std::str::from_utf8(manifest_file.contents()).map_err(|_| {
            KernelWorldError::PackageBinding {
                reason: "kernel-world manifest entry is not valid UTF-8".into(),
            }
        })?;
        let manifest = KernelWorldManifest::parse_toml(manifest_text)?;

        require_package_match("name", &package_manifest.name, &manifest.name)?;
        require_package_match("version", &package_manifest.version, &manifest.version)?;
        require_package_match(
            "architecture",
            &package_manifest.architecture,
            &manifest.machine.guest_architecture,
        )?;
        require_package_match(
            "health.protocol",
            &package_manifest.health.protocol,
            &manifest.lifecycle.health_protocol,
        )?;
        if package_manifest.health.timeout_ms != manifest.lifecycle.health_timeout_ms {
            return package_binding(format!(
                "health timeout differs: package declares {}, world declares {}",
                package_manifest.health.timeout_ms, manifest.lifecycle.health_timeout_ms
            ));
        }

        let mut package_services: Vec<_> = package_manifest
            .services
            .iter()
            .map(|service| (service.name.as_str(), service.protocol.as_str()))
            .collect();
        let mut world_services: Vec<_> = manifest
            .exports
            .iter()
            .map(|export| (export.name.as_str(), export.protocol.as_str()))
            .collect();
        package_services.sort_unstable();
        world_services.sort_unstable();
        if package_services != world_services {
            return package_binding(
                "package services must exactly match kernel-world export names and protocols"
                    .into(),
            );
        }

        let package_requests = normalized_package_requests(package_manifest);
        let world_requests = normalized_world_requests(&manifest);
        if package_requests != world_requests {
            return package_binding(
                "package capability requests must exactly match kernel-world requests".into(),
            );
        }

        if let KernelImage::PackagePayload { path, sha256 } = &manifest.image {
            let image_path = &path[1..];
            let image = package
                .payload_files()
                .iter()
                .find(|file| file.path() == image_path)
                .ok_or_else(|| KernelWorldError::PackageBinding {
                    reason: format!("kernel image `{path}` is absent from the payload"),
                })?;
            let actual = hex::encode(Sha256::digest(image.contents()));
            if actual != *sha256 {
                return package_binding(format!(
                    "kernel image `{path}` SHA-256 mismatch: declared {sha256}, computed {actual}"
                ));
            }
        }

        Ok(Self {
            manifest,
            package_digest: package.digest().clone(),
        })
    }

    pub fn manifest(&self) -> &KernelWorldManifest {
        &self.manifest
    }

    pub fn package_digest(&self) -> &PackageDigest {
        &self.package_digest
    }

    /// Encode the exact verified package/world binding into the bounded
    /// normal form consumed by native admission. Pilot bounds are
    /// intentionally tighter than the hosted schema and are rejected rather
    /// than truncated.
    pub fn encode_native_record(&self) -> Result<NativeKernelWorldRecord, KernelWorldError> {
        NativeKernelWorldRecord::encode(&self.manifest, &self.package_digest)
    }

    pub fn into_instance(self) -> Result<KernelWorldInstance, KernelWorldError> {
        KernelWorldInstance::new(self.manifest, self.package_digest)
    }
}

impl NativeKernelWorldRecordData {
    fn encode(
        manifest: &KernelWorldManifest,
        package_digest: &PackageDigest,
    ) -> Result<Self, KernelWorldError> {
        validate_native_package_digest(package_digest)?;
        let canonical = manifest.canonical_toml()?;
        let canonical_manifest = KernelWorldManifest::parse_toml(&canonical)?;
        enforce_limit(
            "native kernel-world exports",
            canonical_manifest.exports.len() as u64,
            MAX_NATIVE_KERNEL_WORLD_EXPORTS as u64,
        )?;
        enforce_limit(
            "native kernel-world capability requests",
            canonical_manifest.capability_requests.len() as u64,
            MAX_NATIVE_KERNEL_WORLD_CAPABILITY_REQUESTS as u64,
        )?;

        let mut bytes = Vec::with_capacity(1024);
        bytes.extend_from_slice(NATIVE_KERNEL_WORLD_MAGIC);
        push_u16(&mut bytes, NATIVE_KERNEL_WORLD_RECORD_V2);
        push_u16(&mut bytes, 0);
        push_u32(&mut bytes, 0);
        bytes.extend_from_slice(&decode_sha256(package_digest.as_hex(), "package digest")?);
        bytes.extend_from_slice(&Sha256::digest(canonical.as_bytes()));

        bytes.push(match canonical_manifest.integration {
            IntegrationMode::SourceIntegrated => 1,
            IntegrationMode::BinaryContained => 2,
        });
        bytes.push(match canonical_manifest.machine.execution {
            ExecutionMechanism::Paravirtual => 1,
            ExecutionMechanism::HardwareVirtualized => 2,
        });
        bytes.push(match canonical_manifest.machine.firmware {
            FirmwareContract::Direct => 1,
            FirmwareContract::Uefi => 2,
        });
        bytes.push(match canonical_manifest.image {
            KernelImage::PackagePayload { .. } => 1,
            KernelImage::UserSupplied { .. } => 2,
        });
        bytes.push(match canonical_manifest.lifecycle.restart {
            RestartPolicy::Never => 1,
            RestartPolicy::OnFailure => 2,
            RestartPolicy::Always => 3,
        });
        bytes.push(match canonical_manifest.license.redistribution {
            RedistributionPolicy::Redistributable => 1,
            RedistributionPolicy::UserSuppliedOnly => 2,
        });
        bytes.push(u8::from(
            canonical_manifest.license.external_acceptance_required,
        ));
        bytes.push(0);

        push_u16(&mut bytes, canonical_manifest.machine.min_vcpus);
        push_u16(&mut bytes, canonical_manifest.machine.max_vcpus);
        push_u16(&mut bytes, canonical_manifest.quotas.max_devices);
        push_u16(
            &mut bytes,
            canonical_manifest
                .exports
                .len()
                .try_into()
                .map_err(|_| invalid_native_record("export count cannot be represented"))?,
        );
        push_u16(
            &mut bytes,
            canonical_manifest
                .capability_requests
                .len()
                .try_into()
                .map_err(|_| invalid_native_record("request count cannot be represented"))?,
        );
        push_u16(
            &mut bytes,
            canonical_manifest
                .machine
                .requirements
                .len()
                .try_into()
                .map_err(|_| invalid_native_record("requirement count cannot be represented"))?,
        );
        push_u32(
            &mut bytes,
            canonical_manifest.quotas.max_outstanding_requests,
        );
        push_u64(&mut bytes, canonical_manifest.machine.min_memory_mib);
        push_u64(&mut bytes, canonical_manifest.machine.max_memory_mib);
        push_u64(&mut bytes, canonical_manifest.lifecycle.health_timeout_ms);
        push_u64(
            &mut bytes,
            canonical_manifest.quotas.max_requests_per_generation,
        );
        push_u64(
            &mut bytes,
            canonical_manifest.quotas.max_shared_memory_bytes,
        );

        push_string(&mut bytes, &canonical_manifest.name)?;
        push_string(&mut bytes, &canonical_manifest.version)?;
        push_string(&mut bytes, &canonical_manifest.machine.guest_architecture)?;
        push_string(&mut bytes, &canonical_manifest.machine.profile)?;
        push_string(&mut bytes, &canonical_manifest.lifecycle.health_protocol)?;
        match &canonical_manifest.image {
            KernelImage::PackagePayload { path, sha256 } => {
                push_string(&mut bytes, path)?;
                bytes.extend_from_slice(&decode_sha256(sha256, "image.sha256")?);
            }
            KernelImage::UserSupplied { expected_sha256 } => {
                push_string(&mut bytes, "")?;
                bytes.extend_from_slice(&decode_sha256(expected_sha256, "image.expected_sha256")?);
            }
        }

        for requirement in &canonical_manifest.machine.requirements {
            push_string(&mut bytes, requirement)?;
        }
        for export in &canonical_manifest.exports {
            push_string(&mut bytes, &export.name)?;
            bytes.push(match export.plane {
                ExportPlane::Device => 1,
                ExportPlane::Abi => 2,
                ExportPlane::Semantic => 3,
            });
            bytes.push(0);
            push_string(&mut bytes, &export.protocol)?;
            push_string(
                &mut bytes,
                export.authority_request.as_deref().unwrap_or(""),
            )?;
        }
        for request in &canonical_manifest.capability_requests {
            push_string(&mut bytes, &request.kind)?;
            push_string(&mut bytes, &request.purpose)?;
            push_u16(
                &mut bytes,
                request
                    .rights
                    .len()
                    .try_into()
                    .map_err(|_| invalid_native_record("rights count cannot be represented"))?,
            );
            for right in &request.rights {
                if !matches!(right.as_str(), "run" | "stop" | "reset" | "dma") {
                    return Err(invalid_native_record(
                        "right is outside the native pilot vocabulary",
                    ));
                }
                push_string(&mut bytes, right)?;
            }
        }

        enforce_limit(
            "native kernel-world record bytes",
            bytes.len() as u64,
            MAX_NATIVE_KERNEL_WORLD_RECORD_BYTES as u64,
        )?;
        let total_length: u32 = bytes
            .len()
            .try_into()
            .map_err(|_| invalid_native_record("record length cannot be represented"))?;
        bytes[12..16].copy_from_slice(&total_length.to_le_bytes());

        Ok(Self {
            bytes,
            package_digest: package_digest.clone(),
            manifest: canonical_manifest,
        })
    }
}

impl NativeKernelWorldRecord {
    fn encode(
        manifest: &KernelWorldManifest,
        package_digest: &PackageDigest,
    ) -> Result<Self, KernelWorldError> {
        Ok(Self {
            data: NativeKernelWorldRecordData::encode(manifest, package_digest)?,
        })
    }

    /// Validate untrusted bytes without manufacturing verified-package
    /// authority. Prefer calling `InspectedNativeKernelWorldRecord::from_bytes`
    /// directly when the source is explicitly untrusted.
    pub fn inspect(bytes: &[u8]) -> Result<InspectedNativeKernelWorldRecord, KernelWorldError> {
        InspectedNativeKernelWorldRecord::from_bytes(bytes)
    }

    /// Compatibility spelling for inspection. The distinct return type is
    /// intentional: decoding cannot construct a verified admission record.
    pub fn from_bytes(bytes: &[u8]) -> Result<InspectedNativeKernelWorldRecord, KernelWorldError> {
        Self::inspect(bytes)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data.bytes
    }

    pub fn sha256_hex(&self) -> String {
        hex::encode(Sha256::digest(&self.data.bytes))
    }

    pub fn package_digest(&self) -> &PackageDigest {
        &self.data.package_digest
    }

    pub fn manifest(&self) -> &KernelWorldManifest {
        &self.data.manifest
    }
}

impl InspectedNativeKernelWorldRecord {
    /// Validate and inspect an untrusted record. This does not recreate the
    /// verified-package authority that produced an admission record.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, KernelWorldError> {
        enforce_limit(
            "native kernel-world record bytes",
            bytes.len() as u64,
            MAX_NATIVE_KERNEL_WORLD_RECORD_BYTES as u64,
        )?;
        let mut cursor = NativeRecordCursor::new(bytes);
        if cursor.take(8)? != NATIVE_KERNEL_WORLD_MAGIC {
            return Err(invalid_native_record("bad magic"));
        }
        if cursor.u16()? != NATIVE_KERNEL_WORLD_RECORD_V2 {
            return Err(invalid_native_record("unsupported version"));
        }
        if cursor.u16()? != 0 {
            return Err(invalid_native_record("reserved header bits are nonzero"));
        }
        if cursor.u32()? as usize != bytes.len() {
            return Err(invalid_native_record(
                "declared length does not match object",
            ));
        }
        let package_digest = PackageDigest::from_hex(&hex::encode(cursor.take(32)?))
            .map_err(|error| invalid_native_record(&error.to_string()))?;
        validate_native_package_digest(&package_digest)?;
        let declared_manifest_digest = cursor.take(32)?.to_vec();

        let integration = match cursor.u8()? {
            1 => IntegrationMode::SourceIntegrated,
            2 => IntegrationMode::BinaryContained,
            _ => return Err(invalid_native_record("invalid integration enum")),
        };
        let execution = match cursor.u8()? {
            1 => ExecutionMechanism::Paravirtual,
            2 => ExecutionMechanism::HardwareVirtualized,
            _ => return Err(invalid_native_record("invalid execution enum")),
        };
        let firmware = match cursor.u8()? {
            1 => FirmwareContract::Direct,
            2 => FirmwareContract::Uefi,
            _ => return Err(invalid_native_record("invalid firmware enum")),
        };
        let image_kind = cursor.u8()?;
        if image_kind != 1 && image_kind != 2 {
            return Err(invalid_native_record("invalid image enum"));
        }
        let restart = match cursor.u8()? {
            1 => RestartPolicy::Never,
            2 => RestartPolicy::OnFailure,
            3 => RestartPolicy::Always,
            _ => return Err(invalid_native_record("invalid restart enum")),
        };
        let redistribution = match cursor.u8()? {
            1 => RedistributionPolicy::Redistributable,
            2 => RedistributionPolicy::UserSuppliedOnly,
            _ => return Err(invalid_native_record("invalid redistribution enum")),
        };
        let external_acceptance_required = match cursor.u8()? {
            0 => false,
            1 => true,
            _ => return Err(invalid_native_record("invalid acceptance boolean")),
        };
        if cursor.u8()? != 0 {
            return Err(invalid_native_record("reserved enum bits are nonzero"));
        }

        let min_vcpus = cursor.u16()?;
        let max_vcpus = cursor.u16()?;
        let max_devices = cursor.u16()?;
        let export_count = cursor.u16()? as usize;
        let request_count = cursor.u16()? as usize;
        let requirement_count = cursor.u16()? as usize;
        if export_count > MAX_NATIVE_KERNEL_WORLD_EXPORTS {
            return Err(invalid_native_record(
                "native export count exceeds pilot bound",
            ));
        }
        if request_count > MAX_NATIVE_KERNEL_WORLD_CAPABILITY_REQUESTS {
            return Err(invalid_native_record(
                "native request count exceeds pilot bound",
            ));
        }
        if requirement_count > MAX_KERNEL_WORLD_REQUIREMENTS {
            return Err(invalid_native_record("requirement count exceeds bound"));
        }
        let max_outstanding_requests = cursor.u32()?;
        let min_memory_mib = cursor.u64()?;
        let max_memory_mib = cursor.u64()?;
        let health_timeout_ms = cursor.u64()?;
        let max_requests_per_generation = cursor.u64()?;
        let max_shared_memory_bytes = cursor.u64()?;

        let name = cursor.string()?;
        let version = cursor.string()?;
        let guest_architecture = cursor.string()?;
        let profile = cursor.string()?;
        let health_protocol = cursor.string()?;
        let image_path = cursor.string()?;
        let image_sha256 = hex::encode(cursor.take(32)?);

        let mut requirements = Vec::with_capacity(requirement_count);
        for _ in 0..requirement_count {
            requirements.push(cursor.string()?);
        }
        let mut exports = Vec::with_capacity(export_count);
        for _ in 0..export_count {
            let name = cursor.string()?;
            let plane = match cursor.u8()? {
                1 => ExportPlane::Device,
                2 => ExportPlane::Abi,
                3 => ExportPlane::Semantic,
                _ => return Err(invalid_native_record("invalid export plane")),
            };
            if cursor.u8()? != 0 {
                return Err(invalid_native_record("reserved export bits are nonzero"));
            }
            let protocol = cursor.string()?;
            let authority_request = match cursor.string()? {
                authority_request if authority_request.is_empty() => None,
                authority_request => Some(authority_request),
            };
            exports.push(WorldExport {
                name,
                plane,
                protocol,
                authority_request,
            });
        }
        let mut capability_requests = Vec::with_capacity(request_count);
        for _ in 0..request_count {
            let kind = cursor.string()?;
            let purpose = cursor.string()?;
            let rights_count = cursor.u16()? as usize;
            if rights_count > MAX_KERNEL_WORLD_RIGHTS {
                return Err(invalid_native_record("rights count exceeds bound"));
            }
            let mut rights = Vec::with_capacity(rights_count);
            for _ in 0..rights_count {
                rights.push(cursor.string()?);
            }
            capability_requests.push(WorldCapabilityRequest {
                kind,
                rights,
                purpose,
            });
        }
        if !cursor.is_finished() {
            return Err(invalid_native_record("trailing bytes"));
        }

        let image = match image_kind {
            1 => {
                if image_path.is_empty() {
                    return Err(invalid_native_record("package image path is empty"));
                }
                KernelImage::PackagePayload {
                    path: image_path,
                    sha256: image_sha256,
                }
            }
            2 => {
                if !image_path.is_empty() {
                    return Err(invalid_native_record("user image path must be empty"));
                }
                KernelImage::UserSupplied {
                    expected_sha256: image_sha256,
                }
            }
            _ => unreachable!(),
        };
        let manifest = KernelWorldManifest {
            schema: KERNEL_WORLD_SCHEMA_V1.into(),
            name,
            version,
            integration,
            image,
            machine: MachineContract {
                guest_architecture,
                profile,
                execution,
                firmware,
                min_vcpus,
                max_vcpus,
                min_memory_mib,
                max_memory_mib,
                requirements,
            },
            lifecycle: LifecycleContract {
                health_protocol,
                health_timeout_ms,
                restart,
            },
            quotas: ResourceQuotas {
                max_outstanding_requests,
                max_requests_per_generation,
                max_shared_memory_bytes,
                max_devices,
            },
            exports,
            capability_requests,
            license: LicenseContract {
                redistribution,
                external_acceptance_required,
            },
        };
        let canonical = manifest.canonical_toml()?;
        if Sha256::digest(canonical.as_bytes()).as_slice() != declared_manifest_digest {
            return Err(invalid_native_record("canonical manifest digest mismatch"));
        }
        let canonical_record = NativeKernelWorldRecordData::encode(&manifest, &package_digest)?;
        if canonical_record.bytes != bytes {
            return Err(invalid_native_record("record is not canonical"));
        }
        Ok(Self {
            data: canonical_record,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data.bytes
    }

    pub fn sha256_hex(&self) -> String {
        hex::encode(Sha256::digest(&self.data.bytes))
    }

    pub fn package_digest(&self) -> &PackageDigest {
        &self.data.package_digest
    }

    pub fn manifest(&self) -> &KernelWorldManifest {
        &self.data.manifest
    }
}

/// Runtime state common to either integration mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceState {
    Installed,
    Starting,
    Healthy,
    Failed,
    Stopped,
}

/// A generation-bound world identity. Serialized metadata is not authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelWorldIdentity {
    name: String,
    package_digest: PackageDigest,
    generation: u64,
}

impl KernelWorldIdentity {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn package_digest(&self) -> &PackageDigest {
        &self.package_digest
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Bind this verified provider generation beneath a caller-supplied,
    /// registry-allocated execution domain.
    ///
    /// A KernelWorld is an execution domain inside an Ostadix World, not the
    /// distributed World itself. The domain generation and the provider's
    /// lifecycle generation are intentionally distinct: restarting or
    /// reconstructing a provider must not allocate or revive a domain identity.
    /// The resulting record preserves exact package provenance but contains no
    /// bearer token and grants no authority.
    pub fn bind_execution_domain(
        &self,
        domain: DomainIdentity,
    ) -> Result<KernelWorldBinding, WorldIdentityError> {
        KernelWorldBinding::from_descriptive_parts(
            domain,
            self.name.clone(),
            self.package_digest.as_hex(),
            self.generation,
        )
    }
}

/// A request ID is meaningful only with its owning world generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId {
    generation: u64,
    sequence: u64,
}

impl RequestId {
    pub fn generation(self) -> u64 {
        self.generation
    }

    pub fn sequence(self) -> u64 {
        self.sequence
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.generation, self.sequence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestTerminal {
    Replied,
    Cancelled,
    DeadlineExceeded,
    WorldFailed,
    WorldStopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRecord {
    pub request: RequestId,
    pub export: String,
    pub terminal: RequestTerminal,
}

/// Generation-bound description returned when a client resolves an export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportProvenance {
    pub world: KernelWorldIdentity,
    pub integration: IntegrationMode,
    pub export: WorldExport,
}

#[derive(Debug, Clone)]
struct PendingRequest {
    export: String,
}

/// Bounded executable oracle for health gating and one-terminal-result rules.
#[derive(Debug, Clone)]
pub struct KernelWorldInstance {
    manifest: KernelWorldManifest,
    package_digest: PackageDigest,
    generation: u64,
    state: InstanceState,
    issued_in_generation: u64,
    outstanding: BTreeMap<RequestId, PendingRequest>,
    terminal: BTreeMap<RequestId, RequestTerminal>,
}

impl KernelWorldInstance {
    pub fn new(
        manifest: KernelWorldManifest,
        package_digest: PackageDigest,
    ) -> Result<Self, KernelWorldError> {
        manifest.validate()?;
        Ok(Self {
            manifest,
            package_digest,
            generation: 0,
            state: InstanceState::Installed,
            issued_in_generation: 0,
            outstanding: BTreeMap::new(),
            terminal: BTreeMap::new(),
        })
    }

    pub fn manifest(&self) -> &KernelWorldManifest {
        &self.manifest
    }

    pub fn state(&self) -> InstanceState {
        self.state
    }

    pub fn generation(&self) -> Option<u64> {
        (self.generation != 0).then_some(self.generation)
    }

    pub fn outstanding_requests(&self) -> usize {
        self.outstanding.len()
    }

    /// Start the first or a replacement generation. Publication remains gated.
    pub fn start_generation(&mut self) -> Result<KernelWorldIdentity, KernelWorldError> {
        if !matches!(
            self.state,
            InstanceState::Installed | InstanceState::Failed | InstanceState::Stopped
        ) {
            return Err(KernelWorldError::InvalidState {
                action: "start a generation",
                state: self.state,
            });
        }
        let restart_allowed = match self.state {
            InstanceState::Installed => true,
            InstanceState::Failed => matches!(
                self.manifest.lifecycle.restart,
                RestartPolicy::OnFailure | RestartPolicy::Always
            ),
            InstanceState::Stopped => self.manifest.lifecycle.restart == RestartPolicy::Always,
            InstanceState::Starting | InstanceState::Healthy => false,
        };
        if !restart_allowed {
            return Err(KernelWorldError::InvalidState {
                action: "restart under the declared lifecycle policy",
                state: self.state,
            });
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(KernelWorldError::GenerationExhausted)?;
        self.state = InstanceState::Starting;
        self.issued_in_generation = 0;
        self.outstanding.clear();
        self.terminal.clear();
        Ok(self.identity())
    }

    /// Publish exports only after the exact current generation becomes healthy.
    pub fn mark_healthy(&mut self, generation: u64) -> Result<(), KernelWorldError> {
        self.require_generation(generation)?;
        if self.state != InstanceState::Starting {
            return Err(KernelWorldError::InvalidState {
                action: "mark the generation healthy",
                state: self.state,
            });
        }
        self.state = InstanceState::Healthy;
        Ok(())
    }

    /// Resolve one healthy export together with immutable provenance metadata.
    pub fn resolve_export(
        &self,
        generation: u64,
        name: &str,
    ) -> Result<ExportProvenance, KernelWorldError> {
        self.require_generation(generation)?;
        if self.state != InstanceState::Healthy {
            return Err(KernelWorldError::InvalidState {
                action: "resolve an export",
                state: self.state,
            });
        }
        let export = self
            .manifest
            .exports
            .iter()
            .find(|export| export.name == name)
            .cloned()
            .ok_or_else(|| KernelWorldError::UnknownExport {
                name: name.to_owned(),
            })?;
        Ok(ExportProvenance {
            world: self.identity(),
            integration: self.manifest.integration,
            export,
        })
    }

    /// Admit one request only through a currently healthy declared export.
    pub fn begin_request(
        &mut self,
        generation: u64,
        export: &str,
    ) -> Result<RequestId, KernelWorldError> {
        self.resolve_export(generation, export)?;
        if self.outstanding.len() >= self.manifest.quotas.max_outstanding_requests as usize {
            return Err(KernelWorldError::OutstandingRequestLimit);
        }
        if self.issued_in_generation >= self.manifest.quotas.max_requests_per_generation {
            return Err(KernelWorldError::GenerationRequestLimit);
        }
        let sequence = self
            .issued_in_generation
            .checked_add(1)
            .ok_or(KernelWorldError::RequestSequenceExhausted { generation })?;
        self.issued_in_generation = sequence;
        let request = RequestId {
            generation,
            sequence,
        };
        self.outstanding.insert(
            request,
            PendingRequest {
                export: export.to_owned(),
            },
        );
        Ok(request)
    }

    pub fn reply(&mut self, request: RequestId) -> Result<TerminalRecord, KernelWorldError> {
        self.finish_request(request, RequestTerminal::Replied)
    }

    pub fn cancel(&mut self, request: RequestId) -> Result<TerminalRecord, KernelWorldError> {
        self.finish_request(request, RequestTerminal::Cancelled)
    }

    pub fn timeout(&mut self, request: RequestId) -> Result<TerminalRecord, KernelWorldError> {
        self.finish_request(request, RequestTerminal::DeadlineExceeded)
    }

    /// Fail the generation and terminate every outstanding request exactly once.
    pub fn fail_generation(
        &mut self,
        generation: u64,
    ) -> Result<Vec<TerminalRecord>, KernelWorldError> {
        self.require_generation(generation)?;
        if !matches!(self.state, InstanceState::Starting | InstanceState::Healthy) {
            return Err(KernelWorldError::InvalidState {
                action: "fail the generation",
                state: self.state,
            });
        }
        let records = self.drain_requests(RequestTerminal::WorldFailed);
        self.state = InstanceState::Failed;
        Ok(records)
    }

    /// Stop the generation and resolve in-flight requests without implicit retry.
    pub fn stop_generation(
        &mut self,
        generation: u64,
    ) -> Result<Vec<TerminalRecord>, KernelWorldError> {
        self.require_generation(generation)?;
        if !matches!(self.state, InstanceState::Starting | InstanceState::Healthy) {
            return Err(KernelWorldError::InvalidState {
                action: "stop the generation",
                state: self.state,
            });
        }
        let records = self.drain_requests(RequestTerminal::WorldStopped);
        self.state = InstanceState::Stopped;
        Ok(records)
    }

    fn finish_request(
        &mut self,
        request: RequestId,
        terminal: RequestTerminal,
    ) -> Result<TerminalRecord, KernelWorldError> {
        self.require_generation(request.generation)?;
        if let Some(previous) = self.terminal.get(&request) {
            return Err(KernelWorldError::RequestAlreadyTerminal {
                request,
                terminal: *previous,
            });
        }
        let pending = self
            .outstanding
            .remove(&request)
            .ok_or(KernelWorldError::UnknownRequest { request })?;
        self.terminal.insert(request, terminal);
        Ok(TerminalRecord {
            request,
            export: pending.export,
            terminal,
        })
    }

    fn drain_requests(&mut self, terminal: RequestTerminal) -> Vec<TerminalRecord> {
        let outstanding = std::mem::take(&mut self.outstanding);
        outstanding
            .into_iter()
            .map(|(request, pending)| {
                self.terminal.insert(request, terminal);
                TerminalRecord {
                    request,
                    export: pending.export,
                    terminal,
                }
            })
            .collect()
    }

    fn require_generation(&self, generation: u64) -> Result<(), KernelWorldError> {
        if generation != self.generation || generation == 0 {
            return Err(KernelWorldError::StaleGeneration {
                expected: self.generation,
                got: generation,
            });
        }
        Ok(())
    }

    fn identity(&self) -> KernelWorldIdentity {
        KernelWorldIdentity {
            name: self.manifest.name.clone(),
            package_digest: self.package_digest.clone(),
            generation: self.generation,
        }
    }
}

fn enforce_limit(resource: &'static str, actual: u64, limit: u64) -> Result<(), KernelWorldError> {
    if actual > limit {
        return Err(KernelWorldError::LimitExceeded {
            resource,
            limit,
            actual,
        });
    }
    Ok(())
}

fn validate_nonzero_limit(
    field: &'static str,
    value: u64,
    max: u64,
) -> Result<(), KernelWorldError> {
    if value == 0 {
        return Err(KernelWorldError::InvalidField {
            field,
            reason: "must be nonzero".into(),
        });
    }
    enforce_limit(field, value, max)
}

fn validate_range_u16(
    field: &'static str,
    min: u16,
    max: u16,
    ceiling: u16,
) -> Result<(), KernelWorldError> {
    if min == 0 || min > max || max > ceiling {
        return Err(KernelWorldError::InvalidField {
            field,
            reason: format!("must satisfy 1 <= min <= max <= {ceiling}"),
        });
    }
    Ok(())
}

fn validate_range_u64(
    field: &'static str,
    min: u64,
    max: u64,
    ceiling: u64,
) -> Result<(), KernelWorldError> {
    if min == 0 || min > max || max > ceiling {
        return Err(KernelWorldError::InvalidField {
            field,
            reason: format!("must satisfy 1 <= min <= max <= {ceiling}"),
        });
    }
    Ok(())
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), KernelWorldError> {
    enforce_limit(
        field,
        value.len() as u64,
        MAX_KERNEL_WORLD_IDENTIFIER_BYTES as u64,
    )?;
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
    {
        return Err(KernelWorldError::InvalidField {
            field,
            reason: "must contain only ASCII letters, digits, `.`, `_`, `-`, or `+`".into(),
        });
    }
    Ok(())
}

fn is_device_capability_kind(value: &str) -> bool {
    value.len() > "device.".len() && value.starts_with("device.")
}

fn validate_name(field: &'static str, value: &str) -> Result<(), KernelWorldError> {
    enforce_limit(
        field,
        value.len() as u64,
        MAX_KERNEL_WORLD_IDENTIFIER_BYTES as u64,
    )?;
    if value.is_empty() || value.starts_with('/') || value.ends_with('/') {
        return Err(KernelWorldError::InvalidField {
            field,
            reason: "must be a non-empty relative logical name".into(),
        });
    }
    for component in value.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || !component.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+')
            })
        {
            return Err(KernelWorldError::InvalidField {
                field,
                reason: format!("contains unsupported component `{component}`"),
            });
        }
    }
    Ok(())
}

fn validate_protocol(field: &'static str, value: &str) -> Result<(), KernelWorldError> {
    validate_text(field, value)?;
    if value.len() > MAX_KERNEL_WORLD_IDENTIFIER_BYTES {
        return Err(KernelWorldError::LimitExceeded {
            resource: field,
            limit: MAX_KERNEL_WORLD_IDENTIFIER_BYTES as u64,
            actual: value.len() as u64,
        });
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str) -> Result<(), KernelWorldError> {
    enforce_limit(
        field,
        value.len() as u64,
        MAX_KERNEL_WORLD_TEXT_BYTES as u64,
    )?;
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(KernelWorldError::InvalidField {
            field,
            reason: "must be non-empty and contain no control characters".into(),
        });
    }
    Ok(())
}

fn validate_sha256(field: &'static str, value: &str) -> Result<(), KernelWorldError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(KernelWorldError::InvalidField {
            field,
            reason: "must be 64 lowercase hexadecimal characters".into(),
        });
    }
    Ok(())
}

fn validate_payload_path(field: &'static str, value: &str) -> Result<(), KernelWorldError> {
    if !value.starts_with('/') || value == "/" || value.contains('\\') || value.contains('\0') {
        return Err(KernelWorldError::InvalidField {
            field,
            reason: "must be an absolute canonical package-payload path".into(),
        });
    }
    let relative = &value[1..];
    let path = Path::new(relative);
    if path.is_absolute()
        || relative
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(KernelWorldError::InvalidField {
            field,
            reason: "contains an empty, current-directory, or parent-directory component".into(),
        });
    }
    validate_text(field, value)
}

fn package_binding<T>(reason: String) -> Result<T, KernelWorldError> {
    Err(KernelWorldError::PackageBinding { reason })
}

fn require_package_match(
    field: &'static str,
    package: &str,
    world: &str,
) -> Result<(), KernelWorldError> {
    if package != world {
        return package_binding(format!(
            "{field} differs: package declares `{package}`, world declares `{world}`"
        ));
    }
    Ok(())
}

fn normalized_package_requests(
    manifest: &crate::live_system::manifest::PackageManifest,
) -> Vec<(String, String, Vec<String>)> {
    let mut requests: Vec<_> = manifest
        .capability_requests
        .iter()
        .map(|request| {
            let mut rights = request.rights.clone();
            rights.sort();
            (request.kind.clone(), request.purpose.clone(), rights)
        })
        .collect();
    requests.sort();
    requests
}

fn normalized_world_requests(manifest: &KernelWorldManifest) -> Vec<(String, String, Vec<String>)> {
    let mut requests: Vec<_> = manifest
        .capability_requests
        .iter()
        .map(|request| {
            let mut rights = request.rights.clone();
            rights.sort();
            (request.kind.clone(), request.purpose.clone(), rights)
        })
        .collect();
    requests.sort();
    requests
}

fn invalid_native_record(reason: &str) -> KernelWorldError {
    KernelWorldError::InvalidNativeRecord {
        reason: reason.into(),
    }
}

fn validate_native_package_digest(digest: &PackageDigest) -> Result<(), KernelWorldError> {
    if digest.as_hex().bytes().all(|byte| byte == b'0') {
        return Err(invalid_native_record(
            "package digest must not be the all-zero sentinel",
        ));
    }
    Ok(())
}

fn decode_sha256(value: &str, field: &'static str) -> Result<[u8; 32], KernelWorldError> {
    validate_sha256(field, value)?;
    let decoded = hex::decode(value).map_err(|error| KernelWorldError::InvalidField {
        field,
        reason: error.to_string(),
    })?;
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&decoded);
    Ok(digest)
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_string(output: &mut Vec<u8>, value: &str) -> Result<(), KernelWorldError> {
    if value.len() > MAX_KERNEL_WORLD_TEXT_BYTES
        || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
    {
        return Err(invalid_native_record(
            "string is outside the native printable-ASCII/512-byte domain",
        ));
    }
    let length: u16 = value
        .len()
        .try_into()
        .map_err(|_| invalid_native_record("string length cannot be represented"))?;
    push_u16(output, length);
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

struct NativeRecordCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> NativeRecordCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], KernelWorldError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| invalid_native_record("offset overflow"))?;
        if end > self.bytes.len() {
            return Err(invalid_native_record("truncated object"));
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, KernelWorldError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, KernelWorldError> {
        let bytes: [u8; 2] = self
            .take(2)?
            .try_into()
            .map_err(|_| invalid_native_record("truncated u16"))?;
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, KernelWorldError> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| invalid_native_record("truncated u32"))?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, KernelWorldError> {
        let bytes: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| invalid_native_record("truncated u64"))?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn string(&mut self) -> Result<String, KernelWorldError> {
        let length = self.u16()? as usize;
        let bytes = self.take(length)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| invalid_native_record("string is not UTF-8"))
    }

    fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}
