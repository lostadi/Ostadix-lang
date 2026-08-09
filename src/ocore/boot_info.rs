//! Host-side, bounded boot-information normalization.
//!
//! This module defines the architecture-neutral `BootInfoV1` data contract and
//! a strict Multiboot2 decoder for conformance tests and future loader work. It
//! is deliberately not called by the freestanding O-core kernel: parsing a
//! byte slice here is not evidence that any physical firmware or boot path has
//! been implemented.

use std::collections::BTreeSet;

use thiserror::Error;

pub const BOOT_INFO_SCHEMA_V1: u16 = 1;
pub const MULTIBOOT2_BOOTLOADER_MAGIC: u32 = 0x36d7_6289;
pub const MAX_MULTIBOOT2_INFORMATION_BYTES: usize = 1024 * 1024;
pub const MAX_MULTIBOOT2_TAGS: usize = 128;
pub const MAX_MEMORY_REGIONS: usize = 256;
pub const MAX_NORMALIZED_MEMORY_REGIONS: usize = 512;
pub const MAX_MODULES: usize = 32;
pub const MAX_COMMAND_LINE_BYTES: usize = 4096;
pub const MAX_MODULE_COMMAND_LINE_BYTES: usize = 1024;
pub const MAX_BOOTLOADER_NAME_BYTES: usize = 256;
pub const MAX_ARTIFACT_DIGESTS: usize = 32;
pub const MAX_ARTIFACT_NAME_BYTES: usize = 128;
pub const MAX_ACPI_RSDP_BYTES: usize = 4096;
pub const MAX_FRAMEBUFFER_DIMENSION: u32 = 65_536;
pub const MAX_FRAMEBUFFER_BYTES: u64 = 1024 * 1024 * 1024;

const TAG_END: u32 = 0;
const TAG_COMMAND_LINE: u32 = 1;
const TAG_BOOTLOADER_NAME: u32 = 2;
const TAG_MODULE: u32 = 3;
const TAG_BASIC_MEMORY: u32 = 4;
const TAG_BOOT_DEVICE: u32 = 5;
const TAG_MEMORY_MAP: u32 = 6;
const TAG_VBE: u32 = 7;
const TAG_FRAMEBUFFER: u32 = 8;
const TAG_ELF_SECTIONS: u32 = 9;
const TAG_APM: u32 = 10;
const TAG_EFI32_SYSTEM_TABLE: u32 = 11;
const TAG_EFI64_SYSTEM_TABLE: u32 = 12;
const TAG_SMBIOS: u32 = 13;
const TAG_ACPI_OLD: u32 = 14;
const TAG_ACPI_NEW: u32 = 15;
const TAG_NETWORK: u32 = 16;
const TAG_EFI_MEMORY_MAP: u32 = 17;
const TAG_EFI_BOOT_SERVICES: u32 = 18;
const TAG_EFI32_IMAGE_HANDLE: u32 = 19;
const TAG_EFI64_IMAGE_HANDLE: u32 = 20;
const TAG_LOAD_BASE_ADDRESS: u32 = 21;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootProtocolV1 {
    Multiboot2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareProtocolV1 {
    Bios,
    Uefi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootCpuArchitectureV1 {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootCpuV1 {
    pub architecture: BootCpuArchitectureV1,
    pub logical_id: u32,
    pub hardware_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalRangeV1 {
    pub start: u64,
    pub end_exclusive: u64,
}

impl PhysicalRangeV1 {
    pub fn new(start: u64, end_exclusive: u64) -> Result<Self, BootInfoError> {
        if start >= end_exclusive {
            return Err(BootInfoError::InvalidPhysicalRange {
                field: "physical range",
                start,
                end_exclusive,
            });
        }
        Ok(Self {
            start,
            end_exclusive,
        })
    }

    pub fn from_start_and_length(
        field: &'static str,
        start: u64,
        length: u64,
    ) -> Result<Self, BootInfoError> {
        let end_exclusive =
            start
                .checked_add(length)
                .ok_or(BootInfoError::PhysicalRangeOverflow {
                    field,
                    start,
                    length,
                })?;
        if length == 0 {
            return Err(BootInfoError::InvalidPhysicalRange {
                field,
                start,
                end_exclusive,
            });
        }
        Ok(Self {
            start,
            end_exclusive,
        })
    }

    pub fn length(self) -> u64 {
        self.end_exclusive - self.start
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end_exclusive && other.start < self.end_exclusive
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryRegionKindV1 {
    Usable,
    Reserved,
    AcpiReclaimable,
    AcpiNvs,
    BadMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegionV1 {
    pub range: PhysicalRangeV1,
    pub kind: MemoryRegionKindV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootModuleV1 {
    pub range: PhysicalRangeV1,
    pub command_line: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiRsdpKindV1 {
    V1,
    V2OrLater,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpiRsdpV1 {
    pub kind: AcpiRsdpKindV1,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbColorV1 {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramebufferColorMaskV1 {
    pub position: u8,
    pub size: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramebufferFormatV1 {
    Indexed {
        palette: Vec<RgbColorV1>,
    },
    DirectRgb {
        red: FramebufferColorMaskV1,
        green: FramebufferColorMaskV1,
        blue: FramebufferColorMaskV1,
    },
    EgaText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramebufferV1 {
    pub address: u64,
    pub pitch: u32,
    pub width: u32,
    pub height: u32,
    pub bits_per_pixel: u8,
    pub format: FramebufferFormatV1,
}

impl FramebufferV1 {
    pub fn byte_len(&self) -> Result<u64, BootInfoError> {
        u64::from(self.pitch)
            .checked_mul(u64::from(self.height))
            .ok_or(BootInfoError::InvalidFramebuffer(
                "framebuffer byte length overflows",
            ))
    }

    pub fn physical_range(&self) -> Result<PhysicalRangeV1, BootInfoError> {
        PhysicalRangeV1::from_start_and_length("framebuffer", self.address, self.byte_len()?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialTransportV1 {
    PortIo16550,
    Mmio16550,
    Pl011,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerialConsoleV1 {
    pub transport: SerialTransportV1,
    pub base_address: u64,
    pub baud: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDigestV1 {
    pub name: String,
    pub sha256: [u8; 32],
}

/// Trusted loader-side facts that standard Multiboot2 tags cannot encode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Multiboot2ContextV1 {
    pub kernel: PhysicalRangeV1,
    pub boot_cpu: BootCpuV1,
    pub serial: Option<SerialConsoleV1>,
    pub artifact_digests: Vec<ArtifactDigestV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootInfoV1 {
    pub schema_version: u16,
    pub protocol: BootProtocolV1,
    pub firmware: FirmwareProtocolV1,
    pub memory_regions: Vec<MemoryRegionV1>,
    pub modules: Vec<BootModuleV1>,
    pub acpi: Option<AcpiRsdpV1>,
    pub framebuffer: Option<FramebufferV1>,
    pub serial: Option<SerialConsoleV1>,
    pub kernel: PhysicalRangeV1,
    pub command_line: Option<String>,
    pub bootloader_name: Option<String>,
    pub boot_cpu: BootCpuV1,
    pub artifact_digests: Vec<ArtifactDigestV1>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum BootInfoError {
    #[error("invalid Multiboot2 bootloader magic 0x{found:08x}")]
    InvalidMagic { found: u32 },
    #[error("Multiboot2 information address 0x{address:x} is not 8-byte aligned")]
    MisalignedInformationAddress { address: u64 },
    #[error("Multiboot2 information address plus total size overflows")]
    InformationAddressOverflow,
    #[error("Multiboot2 information is shorter than the fixed header and end tag")]
    InformationTooShort,
    #[error("Multiboot2 information is {actual} bytes, exceeding the {maximum}-byte bound")]
    InformationTooLarge { actual: usize, maximum: usize },
    #[error("Multiboot2 total_size {declared} does not match the supplied {actual} bytes")]
    TotalSizeMismatch { declared: usize, actual: usize },
    #[error("Multiboot2 total_size {total_size} is not 8-byte aligned")]
    UnalignedTotalSize { total_size: usize },
    #[error("Multiboot2 tag header at offset {offset} is truncated")]
    TruncatedTagHeader { offset: usize },
    #[error("Multiboot2 tag {tag_type} at offset {offset} has invalid size {size}")]
    InvalidTagSize {
        tag_type: u32,
        offset: usize,
        size: usize,
    },
    #[error("Multiboot2 tag {tag_type} at offset {offset} exceeds total_size")]
    TagOutOfBounds { tag_type: u32, offset: usize },
    #[error("Multiboot2 tag alignment overflows")]
    TagAlignmentOverflow,
    #[error("Multiboot2 information exceeds the {maximum}-tag bound")]
    TooManyTags { maximum: usize },
    #[error("Multiboot2 singleton tag {tag_type} occurs more than once")]
    DuplicateTag { tag_type: u32 },
    #[error("Multiboot2 end tag is missing")]
    MissingEndTag,
    #[error("Multiboot2 end tag is followed by data inside total_size")]
    TrailingDataAfterEndTag,
    #[error("Multiboot2 tag {tag_type} is malformed: {reason}")]
    MalformedTag { tag_type: u32, reason: &'static str },
    #[error("Multiboot2 {field} is not a canonical bounded UTF-8 C string")]
    InvalidString { field: &'static str },
    #[error("Multiboot2 {field} exceeds its {maximum}-byte bound")]
    StringTooLong { field: &'static str, maximum: usize },
    #[error("Multiboot2 memory-map tag is required")]
    MissingMemoryMap,
    #[error("Multiboot2 memory map exceeds the {maximum}-entry bound")]
    TooManyMemoryRegions { maximum: usize },
    #[error("normalized memory map exceeds the {maximum}-entry bound")]
    TooManyNormalizedMemoryRegions { maximum: usize },
    #[error("Multiboot2 memory regions overlap")]
    OverlappingMemoryRegions,
    #[error("Multiboot2 memory map has no usable RAM after reservations")]
    MissingUsableMemory,
    #[error("physical {field} range 0x{start:x}..0x{end_exclusive:x} is empty or reversed")]
    InvalidPhysicalRange {
        field: &'static str,
        start: u64,
        end_exclusive: u64,
    },
    #[error("physical {field} range start 0x{start:x} plus length 0x{length:x} overflows")]
    PhysicalRangeOverflow {
        field: &'static str,
        start: u64,
        length: u64,
    },
    #[error("Multiboot2 module count exceeds the {maximum}-module bound")]
    TooManyModules { maximum: usize },
    #[error("kernel and module physical ranges overlap")]
    OverlappingLoadedRanges,
    #[error("loaded {field} range is not completely covered by non-bad firmware memory")]
    UncoveredLoadedRange { field: &'static str },
    #[error("ACPI RSDP is malformed: {0}")]
    InvalidAcpi(&'static str),
    #[error("old and new ACPI tags describe different RSDP roots")]
    ConflictingAcpiTags,
    #[error("framebuffer tag is malformed: {0}")]
    InvalidFramebuffer(&'static str),
    #[error("Multiboot2 information contains conflicting EFI32 and EFI64 evidence")]
    ConflictingFirmwareEvidence,
    #[error("Multiboot2 context must describe an x86_64 boot CPU")]
    InvalidBootCpuArchitecture,
    #[error("serial console description is invalid: {0}")]
    InvalidSerial(&'static str),
    #[error("at least one artifact SHA-256 is required")]
    MissingArtifactDigest,
    #[error("artifact digest count exceeds the {maximum}-entry bound")]
    TooManyArtifactDigests { maximum: usize },
    #[error("artifact name is invalid")]
    InvalidArtifactName,
    #[error("artifact name {name:?} occurs more than once")]
    DuplicateArtifactName { name: String },
    #[error("artifact {name:?} uses the all-zero SHA-256 sentinel")]
    ZeroArtifactDigest { name: String },
}

/// Parse an exact Multiboot2 information record into owned, bounded
/// `BootInfoV1` state.
///
/// `information_address` is the physical address passed by the bootloader. It
/// is kept separate from the host slice address so conformance fixtures can
/// exercise the architectural alignment rule without relying on allocator
/// alignment.
pub fn parse_multiboot2(
    magic: u32,
    information_address: u64,
    bytes: &[u8],
    context: &Multiboot2ContextV1,
) -> Result<BootInfoV1, BootInfoError> {
    if magic != MULTIBOOT2_BOOTLOADER_MAGIC {
        return Err(BootInfoError::InvalidMagic { found: magic });
    }
    if information_address & 7 != 0 {
        return Err(BootInfoError::MisalignedInformationAddress {
            address: information_address,
        });
    }
    if bytes.len() < 16 {
        return Err(BootInfoError::InformationTooShort);
    }
    if bytes.len() > MAX_MULTIBOOT2_INFORMATION_BYTES {
        return Err(BootInfoError::InformationTooLarge {
            actual: bytes.len(),
            maximum: MAX_MULTIBOOT2_INFORMATION_BYTES,
        });
    }

    let total_size = read_u32(bytes, 0) as usize;
    if total_size != bytes.len() {
        return Err(BootInfoError::TotalSizeMismatch {
            declared: total_size,
            actual: bytes.len(),
        });
    }
    if total_size & 7 != 0 {
        return Err(BootInfoError::UnalignedTotalSize { total_size });
    }
    let total_size_u64 =
        u64::try_from(total_size).map_err(|_| BootInfoError::InformationAddressOverflow)?;
    information_address
        .checked_add(total_size_u64)
        .ok_or(BootInfoError::InformationAddressOverflow)?;

    let (kernel, serial, artifact_digests) = validate_context(context)?;

    let mut seen_singletons = [false; 22];
    let mut offset = 8usize;
    let mut tag_count = 0usize;
    let mut found_end = false;
    let mut command_line = None;
    let mut bootloader_name = None;
    let mut raw_memory_regions = None;
    let mut modules = Vec::new();
    let mut framebuffer = None;
    let mut acpi_old = None;
    let mut acpi_new = None;
    let mut efi32 = false;
    let mut efi64 = false;
    let mut any_efi = false;

    while offset < total_size {
        if offset & 7 != 0 || total_size - offset < 8 {
            return Err(BootInfoError::TruncatedTagHeader { offset });
        }
        tag_count += 1;
        if tag_count > MAX_MULTIBOOT2_TAGS {
            return Err(BootInfoError::TooManyTags {
                maximum: MAX_MULTIBOOT2_TAGS,
            });
        }

        let tag_type = read_u32(bytes, offset);
        let tag_size = read_u32(bytes, offset + 4) as usize;
        if tag_size < 8 {
            return Err(BootInfoError::InvalidTagSize {
                tag_type,
                offset,
                size: tag_size,
            });
        }
        let tag_end = offset
            .checked_add(tag_size)
            .ok_or(BootInfoError::TagOutOfBounds { tag_type, offset })?;
        if tag_end > total_size {
            return Err(BootInfoError::TagOutOfBounds { tag_type, offset });
        }
        let tag = &bytes[offset..tag_end];

        if tag_type == TAG_END {
            if tag_size != 8 {
                return Err(BootInfoError::InvalidTagSize {
                    tag_type,
                    offset,
                    size: tag_size,
                });
            }
            if tag_end != total_size {
                return Err(BootInfoError::TrailingDataAfterEndTag);
            }
            found_end = true;
            break;
        }

        if is_singleton_tag(tag_type) {
            let seen = &mut seen_singletons[tag_type as usize];
            if *seen {
                return Err(BootInfoError::DuplicateTag { tag_type });
            }
            *seen = true;
        }

        match tag_type {
            TAG_COMMAND_LINE => {
                command_line = Some(parse_c_string(
                    &tag[8..],
                    MAX_COMMAND_LINE_BYTES,
                    "command line",
                )?);
            }
            TAG_BOOTLOADER_NAME => {
                bootloader_name = Some(parse_c_string(
                    &tag[8..],
                    MAX_BOOTLOADER_NAME_BYTES,
                    "bootloader name",
                )?);
            }
            TAG_MODULE => {
                if modules.len() >= MAX_MODULES {
                    return Err(BootInfoError::TooManyModules {
                        maximum: MAX_MODULES,
                    });
                }
                if tag_size < 17 {
                    return Err(malformed(tag_type, "module tag is too short"));
                }
                let start = u64::from(read_u32(tag, 8));
                let end_exclusive = u64::from(read_u32(tag, 12));
                let range = checked_range("module", start, end_exclusive)?;
                let module_command_line = parse_c_string(
                    &tag[16..],
                    MAX_MODULE_COMMAND_LINE_BYTES,
                    "module command line",
                )?;
                modules.push(BootModuleV1 {
                    range,
                    command_line: module_command_line,
                });
            }
            TAG_BASIC_MEMORY => require_exact_size(tag_type, tag_size, 16)?,
            TAG_BOOT_DEVICE => require_exact_size(tag_type, tag_size, 20)?,
            TAG_MEMORY_MAP => raw_memory_regions = Some(parse_memory_map(tag)?),
            TAG_VBE => require_exact_size(tag_type, tag_size, 784)?,
            TAG_FRAMEBUFFER => framebuffer = Some(parse_framebuffer(tag)?),
            TAG_ELF_SECTIONS => validate_elf_sections(tag)?,
            TAG_APM => require_exact_size(tag_type, tag_size, 28)?,
            TAG_EFI32_SYSTEM_TABLE | TAG_EFI32_IMAGE_HANDLE => {
                require_exact_size(tag_type, tag_size, 12)?;
                if read_u32(tag, 8) == 0 {
                    return Err(malformed(tag_type, "EFI32 pointer is zero"));
                }
                efi32 = true;
                any_efi = true;
            }
            TAG_EFI64_SYSTEM_TABLE | TAG_EFI64_IMAGE_HANDLE => {
                require_exact_size(tag_type, tag_size, 16)?;
                if read_u64(tag, 8) == 0 {
                    return Err(malformed(tag_type, "EFI64 pointer is zero"));
                }
                efi64 = true;
                any_efi = true;
            }
            TAG_SMBIOS => {
                if tag_size < 16 {
                    return Err(malformed(tag_type, "SMBIOS tag is too short"));
                }
            }
            TAG_ACPI_OLD => acpi_old = Some(parse_acpi_old(&tag[8..])?),
            TAG_ACPI_NEW => acpi_new = Some(parse_acpi_new(&tag[8..])?),
            TAG_NETWORK => {
                if tag_size == 8 {
                    return Err(malformed(tag_type, "network payload is empty"));
                }
            }
            TAG_EFI_MEMORY_MAP => {
                validate_efi_memory_map(tag)?;
                any_efi = true;
            }
            TAG_EFI_BOOT_SERVICES => {
                require_exact_size(tag_type, tag_size, 8)?;
                any_efi = true;
            }
            TAG_LOAD_BASE_ADDRESS => require_exact_size(tag_type, tag_size, 12)?,
            _ => {}
        }

        offset = align_up_8(tag_end)?;
        if offset > total_size {
            return Err(BootInfoError::TagOutOfBounds { tag_type, offset });
        }
    }

    if !found_end {
        return Err(BootInfoError::MissingEndTag);
    }
    if efi32 && efi64 {
        return Err(BootInfoError::ConflictingFirmwareEvidence);
    }

    let firmware = if any_efi {
        FirmwareProtocolV1::Uefi
    } else {
        FirmwareProtocolV1::Bios
    };
    let acpi = reconcile_acpi(acpi_old, acpi_new)?;
    let mut memory_regions = raw_memory_regions.ok_or(BootInfoError::MissingMemoryMap)?;

    validate_loaded_ranges(kernel, &modules)?;
    ensure_range_covered(kernel, &memory_regions, "kernel")?;
    memory_regions = reserve_range(memory_regions, kernel)?;
    for module in &modules {
        ensure_range_covered(module.range, &memory_regions, "module")?;
        memory_regions = reserve_range(memory_regions, module.range)?;
    }
    if let Some(framebuffer) = &framebuffer {
        memory_regions = reserve_intersections(memory_regions, framebuffer.physical_range()?)?;
    }
    if !memory_regions
        .iter()
        .any(|region| region.kind == MemoryRegionKindV1::Usable)
    {
        return Err(BootInfoError::MissingUsableMemory);
    }

    Ok(BootInfoV1 {
        schema_version: BOOT_INFO_SCHEMA_V1,
        protocol: BootProtocolV1::Multiboot2,
        firmware,
        memory_regions,
        modules,
        acpi,
        framebuffer,
        serial,
        kernel,
        command_line,
        bootloader_name,
        boot_cpu: context.boot_cpu,
        artifact_digests,
    })
}

fn validate_context(
    context: &Multiboot2ContextV1,
) -> Result<
    (
        PhysicalRangeV1,
        Option<SerialConsoleV1>,
        Vec<ArtifactDigestV1>,
    ),
    BootInfoError,
> {
    let kernel = checked_range("kernel", context.kernel.start, context.kernel.end_exclusive)?;
    if context.boot_cpu.architecture != BootCpuArchitectureV1::X86_64 {
        return Err(BootInfoError::InvalidBootCpuArchitecture);
    }
    if let Some(serial) = context.serial {
        validate_serial(serial)?;
    }

    if context.artifact_digests.is_empty() {
        return Err(BootInfoError::MissingArtifactDigest);
    }
    if context.artifact_digests.len() > MAX_ARTIFACT_DIGESTS {
        return Err(BootInfoError::TooManyArtifactDigests {
            maximum: MAX_ARTIFACT_DIGESTS,
        });
    }
    let mut names = BTreeSet::new();
    let mut artifact_digests = context.artifact_digests.clone();
    for artifact in &artifact_digests {
        if !valid_artifact_name(&artifact.name) {
            return Err(BootInfoError::InvalidArtifactName);
        }
        if !names.insert(artifact.name.clone()) {
            return Err(BootInfoError::DuplicateArtifactName {
                name: artifact.name.clone(),
            });
        }
        if artifact.sha256.iter().all(|byte| *byte == 0) {
            return Err(BootInfoError::ZeroArtifactDigest {
                name: artifact.name.clone(),
            });
        }
    }
    artifact_digests.sort_by(|left, right| left.name.cmp(&right.name));
    Ok((kernel, context.serial, artifact_digests))
}

fn valid_artifact_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_ARTIFACT_NAME_BYTES {
        return false;
    }
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_serial(serial: SerialConsoleV1) -> Result<(), BootInfoError> {
    if serial.base_address == 0 {
        return Err(BootInfoError::InvalidSerial("base address is zero"));
    }
    if serial.baud == 0 || serial.baud > 4_000_000 {
        return Err(BootInfoError::InvalidSerial("baud is outside bounds"));
    }
    match serial.transport {
        SerialTransportV1::PortIo16550 if serial.base_address > u64::from(u16::MAX) => Err(
            BootInfoError::InvalidSerial("port-I/O base does not fit in 16 bits"),
        ),
        SerialTransportV1::Mmio16550 | SerialTransportV1::Pl011 if serial.base_address & 3 != 0 => {
            Err(BootInfoError::InvalidSerial(
                "MMIO base is not 4-byte aligned",
            ))
        }
        _ => Ok(()),
    }
}

fn parse_memory_map(tag: &[u8]) -> Result<Vec<MemoryRegionV1>, BootInfoError> {
    if tag.len() < 40 {
        return Err(malformed(TAG_MEMORY_MAP, "memory map has no entry"));
    }
    let entry_size = read_u32(tag, 8) as usize;
    let entry_version = read_u32(tag, 12);
    if entry_size < 24 || entry_size & 7 != 0 {
        return Err(malformed(
            TAG_MEMORY_MAP,
            "entry_size is less than 24 or not a multiple of 8",
        ));
    }
    if entry_version != 0 {
        return Err(malformed(
            TAG_MEMORY_MAP,
            "unsupported memory-map entry version",
        ));
    }
    let entries = &tag[16..];
    if entries.is_empty() || !entries.len().is_multiple_of(entry_size) {
        return Err(malformed(
            TAG_MEMORY_MAP,
            "entry payload is empty or not divisible by entry_size",
        ));
    }
    let count = entries.len() / entry_size;
    if count > MAX_MEMORY_REGIONS {
        return Err(BootInfoError::TooManyMemoryRegions {
            maximum: MAX_MEMORY_REGIONS,
        });
    }

    let mut regions = Vec::with_capacity(count);
    for entry in entries.chunks_exact(entry_size) {
        let start = read_u64(entry, 0);
        let length = read_u64(entry, 8);
        let range = PhysicalRangeV1::from_start_and_length("memory region", start, length)?;
        let kind = match read_u32(entry, 16) {
            1 => MemoryRegionKindV1::Usable,
            3 => MemoryRegionKindV1::AcpiReclaimable,
            4 => MemoryRegionKindV1::AcpiNvs,
            5 => MemoryRegionKindV1::BadMemory,
            _ => MemoryRegionKindV1::Reserved,
        };
        regions.push(MemoryRegionV1 { range, kind });
    }
    regions.sort_by_key(|region| (region.range.start, region.range.end_exclusive));
    if regions
        .windows(2)
        .any(|pair| pair[0].range.end_exclusive > pair[1].range.start)
    {
        return Err(BootInfoError::OverlappingMemoryRegions);
    }
    if !regions
        .iter()
        .any(|region| region.kind == MemoryRegionKindV1::Usable)
    {
        return Err(BootInfoError::MissingUsableMemory);
    }
    Ok(merge_adjacent(regions))
}

fn parse_framebuffer(tag: &[u8]) -> Result<FramebufferV1, BootInfoError> {
    if tag.len() < 32 {
        return Err(BootInfoError::InvalidFramebuffer(
            "common framebuffer fields are truncated",
        ));
    }
    let address = read_u64(tag, 8);
    let pitch = read_u32(tag, 16);
    let width = read_u32(tag, 20);
    let height = read_u32(tag, 24);
    let bits_per_pixel = tag[28];
    let framebuffer_type = tag[29];
    if address == 0 || pitch == 0 || width == 0 || height == 0 || bits_per_pixel == 0 {
        return Err(BootInfoError::InvalidFramebuffer(
            "address, pitch, dimensions, and depth must be nonzero",
        ));
    }
    if width > MAX_FRAMEBUFFER_DIMENSION || height > MAX_FRAMEBUFFER_DIMENSION {
        return Err(BootInfoError::InvalidFramebuffer(
            "framebuffer dimensions exceed bounds",
        ));
    }

    let format = match framebuffer_type {
        0 => {
            if tag.len() < 34 {
                return Err(BootInfoError::InvalidFramebuffer(
                    "indexed palette header is truncated",
                ));
            }
            if bits_per_pixel > 8 {
                return Err(BootInfoError::InvalidFramebuffer(
                    "indexed depth exceeds 8 bits",
                ));
            }
            let color_count = usize::from(read_u16(tag, 32));
            if color_count == 0 || color_count > 256 || color_count > (1usize << bits_per_pixel) {
                return Err(BootInfoError::InvalidFramebuffer(
                    "indexed palette count is outside bounds",
                ));
            }
            let palette_bytes = color_count
                .checked_mul(3)
                .and_then(|bytes| 34usize.checked_add(bytes))
                .ok_or(BootInfoError::InvalidFramebuffer(
                    "indexed palette length overflows",
                ))?;
            if tag.len() != palette_bytes {
                return Err(BootInfoError::InvalidFramebuffer(
                    "indexed palette length is not exact",
                ));
            }
            let palette = tag[34..]
                .chunks_exact(3)
                .map(|color| RgbColorV1 {
                    red: color[0],
                    green: color[1],
                    blue: color[2],
                })
                .collect();
            FramebufferFormatV1::Indexed { palette }
        }
        1 => {
            if tag.len() != 38 {
                return Err(BootInfoError::InvalidFramebuffer(
                    "direct-RGB color fields are not exact",
                ));
            }
            if bits_per_pixel > 64 {
                return Err(BootInfoError::InvalidFramebuffer(
                    "direct-RGB depth exceeds 64 bits",
                ));
            }
            let red = FramebufferColorMaskV1 {
                position: tag[32],
                size: tag[33],
            };
            let green = FramebufferColorMaskV1 {
                position: tag[34],
                size: tag[35],
            };
            let blue = FramebufferColorMaskV1 {
                position: tag[36],
                size: tag[37],
            };
            validate_color_masks(bits_per_pixel, red, green, blue)?;
            FramebufferFormatV1::DirectRgb { red, green, blue }
        }
        2 => {
            if tag.len() != 32 || bits_per_pixel != 16 {
                return Err(BootInfoError::InvalidFramebuffer(
                    "EGA text mode must have exact common fields and 16-bit cells",
                ));
            }
            FramebufferFormatV1::EgaText
        }
        _ => {
            return Err(BootInfoError::InvalidFramebuffer(
                "unknown framebuffer type",
            ));
        }
    };

    let row_bits = u64::from(width)
        .checked_mul(u64::from(bits_per_pixel))
        .ok_or(BootInfoError::InvalidFramebuffer("row width overflows"))?;
    let minimum_pitch = row_bits
        .checked_add(7)
        .ok_or(BootInfoError::InvalidFramebuffer("row width overflows"))?
        / 8;
    if u64::from(pitch) < minimum_pitch {
        return Err(BootInfoError::InvalidFramebuffer(
            "pitch is shorter than one row",
        ));
    }
    let byte_len = u64::from(pitch).checked_mul(u64::from(height)).ok_or(
        BootInfoError::InvalidFramebuffer("framebuffer byte length overflows"),
    )?;
    if byte_len > MAX_FRAMEBUFFER_BYTES {
        return Err(BootInfoError::InvalidFramebuffer(
            "framebuffer byte length exceeds bounds",
        ));
    }
    address
        .checked_add(byte_len)
        .ok_or(BootInfoError::InvalidFramebuffer(
            "framebuffer physical range overflows",
        ))?;

    Ok(FramebufferV1 {
        address,
        pitch,
        width,
        height,
        bits_per_pixel,
        format,
    })
}

fn validate_color_masks(
    bits_per_pixel: u8,
    red: FramebufferColorMaskV1,
    green: FramebufferColorMaskV1,
    blue: FramebufferColorMaskV1,
) -> Result<(), BootInfoError> {
    fn mask(bits_per_pixel: u8, color: FramebufferColorMaskV1) -> Option<u64> {
        if color.size == 0 {
            return None;
        }
        let end = color.position.checked_add(color.size)?;
        if end > bits_per_pixel || end > 64 {
            return None;
        }
        let low = if color.size == 64 {
            u64::MAX
        } else {
            (1u64 << color.size) - 1
        };
        Some(low << color.position)
    }

    let red = mask(bits_per_pixel, red).ok_or(BootInfoError::InvalidFramebuffer(
        "red color mask is invalid",
    ))?;
    let green = mask(bits_per_pixel, green).ok_or(BootInfoError::InvalidFramebuffer(
        "green color mask is invalid",
    ))?;
    let blue = mask(bits_per_pixel, blue).ok_or(BootInfoError::InvalidFramebuffer(
        "blue color mask is invalid",
    ))?;
    if red & green != 0 || red & blue != 0 || green & blue != 0 {
        return Err(BootInfoError::InvalidFramebuffer(
            "direct-RGB color masks overlap",
        ));
    }
    Ok(())
}

fn parse_acpi_old(bytes: &[u8]) -> Result<AcpiRsdpV1, BootInfoError> {
    if bytes.len() != 20 {
        return Err(BootInfoError::InvalidAcpi(
            "ACPI 1.0 RSDP must be exactly 20 bytes",
        ));
    }
    validate_rsdp_prefix(bytes)?;
    Ok(AcpiRsdpV1 {
        kind: AcpiRsdpKindV1::V1,
        bytes: bytes.to_vec(),
    })
}

fn parse_acpi_new(bytes: &[u8]) -> Result<AcpiRsdpV1, BootInfoError> {
    if bytes.len() < 36 || bytes.len() > MAX_ACPI_RSDP_BYTES {
        return Err(BootInfoError::InvalidAcpi(
            "ACPI 2.0 RSDP length is outside bounds",
        ));
    }
    validate_rsdp_prefix(bytes)?;
    if bytes[15] < 2 {
        return Err(BootInfoError::InvalidAcpi(
            "new ACPI tag carries an old revision",
        ));
    }
    let declared = read_u32(bytes, 20) as usize;
    if declared != bytes.len() || !(36..=MAX_ACPI_RSDP_BYTES).contains(&declared) {
        return Err(BootInfoError::InvalidAcpi(
            "ACPI 2.0 RSDP declared length is not exact",
        ));
    }
    if checksum(bytes) != 0 {
        return Err(BootInfoError::InvalidAcpi(
            "ACPI 2.0 extended checksum is invalid",
        ));
    }
    Ok(AcpiRsdpV1 {
        kind: AcpiRsdpKindV1::V2OrLater,
        bytes: bytes.to_vec(),
    })
}

fn validate_rsdp_prefix(bytes: &[u8]) -> Result<(), BootInfoError> {
    if &bytes[..8] != b"RSD PTR " {
        return Err(BootInfoError::InvalidAcpi("RSDP signature is invalid"));
    }
    if checksum(&bytes[..20]) != 0 {
        return Err(BootInfoError::InvalidAcpi("ACPI 1.0 checksum is invalid"));
    }
    Ok(())
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}

fn reconcile_acpi(
    old: Option<AcpiRsdpV1>,
    new: Option<AcpiRsdpV1>,
) -> Result<Option<AcpiRsdpV1>, BootInfoError> {
    match (old, new) {
        (Some(old), Some(new)) => {
            if old.bytes != new.bytes[..20] {
                return Err(BootInfoError::ConflictingAcpiTags);
            }
            Ok(Some(new))
        }
        (Some(old), None) => Ok(Some(old)),
        (None, Some(new)) => Ok(Some(new)),
        (None, None) => Ok(None),
    }
}

fn validate_efi_memory_map(tag: &[u8]) -> Result<(), BootInfoError> {
    if tag.len() < 56 {
        return Err(malformed(
            TAG_EFI_MEMORY_MAP,
            "EFI memory map has no complete descriptor",
        ));
    }
    let descriptor_size = read_u32(tag, 8) as usize;
    if descriptor_size < 40 || descriptor_size & 7 != 0 {
        return Err(malformed(
            TAG_EFI_MEMORY_MAP,
            "EFI descriptor size is less than 40 or not 8-byte aligned",
        ));
    }
    if !(tag.len() - 16).is_multiple_of(descriptor_size) {
        return Err(malformed(
            TAG_EFI_MEMORY_MAP,
            "EFI descriptor payload is not exact",
        ));
    }
    Ok(())
}

fn validate_elf_sections(tag: &[u8]) -> Result<(), BootInfoError> {
    if tag.len() < 20 {
        return Err(malformed(TAG_ELF_SECTIONS, "ELF section tag is too short"));
    }
    let count = read_u32(tag, 8) as usize;
    let entry_size = read_u32(tag, 12) as usize;
    let string_index = read_u32(tag, 16) as usize;
    if count == 0 || entry_size < 40 || string_index >= count {
        return Err(malformed(
            TAG_ELF_SECTIONS,
            "ELF section metadata is invalid",
        ));
    }
    let expected = count
        .checked_mul(entry_size)
        .and_then(|size| 20usize.checked_add(size))
        .ok_or_else(|| malformed(TAG_ELF_SECTIONS, "ELF section length overflows"))?;
    if expected != tag.len() {
        return Err(malformed(
            TAG_ELF_SECTIONS,
            "ELF section payload is not exact",
        ));
    }
    Ok(())
}

fn validate_loaded_ranges(
    kernel: PhysicalRangeV1,
    modules: &[BootModuleV1],
) -> Result<(), BootInfoError> {
    let mut ranges = Vec::with_capacity(modules.len() + 1);
    ranges.push(kernel);
    ranges.extend(modules.iter().map(|module| module.range));
    ranges.sort_by_key(|range| (range.start, range.end_exclusive));
    if ranges
        .windows(2)
        .any(|pair| pair[0].end_exclusive > pair[1].start)
    {
        return Err(BootInfoError::OverlappingLoadedRanges);
    }
    Ok(())
}

fn ensure_range_covered(
    range: PhysicalRangeV1,
    regions: &[MemoryRegionV1],
    field: &'static str,
) -> Result<(), BootInfoError> {
    let mut cursor = range.start;
    for region in regions {
        if region.range.end_exclusive <= cursor {
            continue;
        }
        if region.range.start > cursor || region.kind == MemoryRegionKindV1::BadMemory {
            break;
        }
        cursor = region.range.end_exclusive.min(range.end_exclusive);
        if cursor == range.end_exclusive {
            return Ok(());
        }
    }
    Err(BootInfoError::UncoveredLoadedRange { field })
}

fn reserve_range(
    regions: Vec<MemoryRegionV1>,
    reserved: PhysicalRangeV1,
) -> Result<Vec<MemoryRegionV1>, BootInfoError> {
    let regions = reserve_intersections(regions, reserved)?;
    if !regions.iter().any(|region| {
        region.kind == MemoryRegionKindV1::Reserved
            && region.range.start <= reserved.start
            && region.range.end_exclusive >= reserved.end_exclusive
    }) {
        return Err(BootInfoError::UncoveredLoadedRange { field: "loaded" });
    }
    Ok(regions)
}

fn reserve_intersections(
    regions: Vec<MemoryRegionV1>,
    reserved: PhysicalRangeV1,
) -> Result<Vec<MemoryRegionV1>, BootInfoError> {
    let mut output = Vec::with_capacity(regions.len().saturating_add(2));
    for region in regions {
        if !region.range.overlaps(reserved) || region.kind == MemoryRegionKindV1::BadMemory {
            output.push(region);
            continue;
        }
        if region.range.start < reserved.start {
            output.push(MemoryRegionV1 {
                range: checked_range(
                    "memory region",
                    region.range.start,
                    reserved.start.min(region.range.end_exclusive),
                )?,
                kind: region.kind,
            });
        }
        let intersection_start = region.range.start.max(reserved.start);
        let intersection_end = region.range.end_exclusive.min(reserved.end_exclusive);
        output.push(MemoryRegionV1 {
            range: checked_range("reserved memory", intersection_start, intersection_end)?,
            kind: MemoryRegionKindV1::Reserved,
        });
        if reserved.end_exclusive < region.range.end_exclusive {
            output.push(MemoryRegionV1 {
                range: checked_range(
                    "memory region",
                    reserved.end_exclusive.max(region.range.start),
                    region.range.end_exclusive,
                )?,
                kind: region.kind,
            });
        }
        if output.len() > MAX_NORMALIZED_MEMORY_REGIONS {
            return Err(BootInfoError::TooManyNormalizedMemoryRegions {
                maximum: MAX_NORMALIZED_MEMORY_REGIONS,
            });
        }
    }
    Ok(merge_adjacent(output))
}

fn merge_adjacent(regions: Vec<MemoryRegionV1>) -> Vec<MemoryRegionV1> {
    let mut merged: Vec<MemoryRegionV1> = Vec::with_capacity(regions.len());
    for region in regions {
        if let Some(previous) = merged.last_mut() {
            if previous.kind == region.kind && previous.range.end_exclusive == region.range.start {
                previous.range.end_exclusive = region.range.end_exclusive;
                continue;
            }
        }
        merged.push(region);
    }
    merged
}

fn parse_c_string(
    bytes: &[u8],
    maximum: usize,
    field: &'static str,
) -> Result<String, BootInfoError> {
    if bytes.len() > maximum.saturating_add(1) {
        return Err(BootInfoError::StringTooLong { field, maximum });
    }
    let Some(nul) = bytes.iter().position(|byte| *byte == 0) else {
        return Err(BootInfoError::InvalidString { field });
    };
    if nul + 1 != bytes.len() {
        return Err(BootInfoError::InvalidString { field });
    }
    std::str::from_utf8(&bytes[..nul])
        .map(str::to_owned)
        .map_err(|_| BootInfoError::InvalidString { field })
}

fn checked_range(
    field: &'static str,
    start: u64,
    end_exclusive: u64,
) -> Result<PhysicalRangeV1, BootInfoError> {
    if start >= end_exclusive {
        return Err(BootInfoError::InvalidPhysicalRange {
            field,
            start,
            end_exclusive,
        });
    }
    Ok(PhysicalRangeV1 {
        start,
        end_exclusive,
    })
}

fn is_singleton_tag(tag_type: u32) -> bool {
    tag_type <= TAG_LOAD_BASE_ADDRESS && !matches!(tag_type, TAG_END | TAG_MODULE | TAG_NETWORK)
}

fn require_exact_size(tag_type: u32, actual: usize, expected: usize) -> Result<(), BootInfoError> {
    if actual != expected {
        return Err(malformed(tag_type, "tag size is not exact"));
    }
    Ok(())
}

fn malformed(tag_type: u32, reason: &'static str) -> BootInfoError {
    BootInfoError::MalformedTag { tag_type, reason }
}

fn align_up_8(value: usize) -> Result<usize, BootInfoError> {
    value
        .checked_add(7)
        .map(|value| value & !7)
        .ok_or(BootInfoError::TagAlignmentOverflow)
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("checked slice"))
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("checked slice"))
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("checked slice"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Multiboot2ContextV1 {
        Multiboot2ContextV1 {
            kernel: PhysicalRangeV1 {
                start: 0x10_0000,
                end_exclusive: 0x12_0000,
            },
            boot_cpu: BootCpuV1 {
                architecture: BootCpuArchitectureV1::X86_64,
                logical_id: 0,
                hardware_id: 7,
            },
            serial: Some(SerialConsoleV1 {
                transport: SerialTransportV1::PortIo16550,
                base_address: 0x3f8,
                baud: 115_200,
            }),
            artifact_digests: vec![ArtifactDigestV1 {
                name: "okernel".into(),
                sha256: [0xa5; 32],
            }],
        }
    }

    fn push_tag(bytes: &mut Vec<u8>, tag_type: u32, payload: &[u8]) {
        bytes.extend_from_slice(&tag_type.to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(8 + payload.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(payload);
        while bytes.len() & 7 != 0 {
            bytes.push(0xcc);
        }
    }

    fn mmap_payload(entries: &[(u64, u64, u32)]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&24u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        for (start, length, kind) in entries {
            payload.extend_from_slice(&start.to_le_bytes());
            payload.extend_from_slice(&length.to_le_bytes());
            payload.extend_from_slice(&kind.to_le_bytes());
            payload.extend_from_slice(&0u32.to_le_bytes());
        }
        payload
    }

    fn finish(mut bytes: Vec<u8>) -> Vec<u8> {
        push_tag(&mut bytes, TAG_END, &[]);
        let total = u32::try_from(bytes.len()).unwrap();
        bytes[..4].copy_from_slice(&total.to_le_bytes());
        bytes
    }

    fn minimal_information() -> Vec<u8> {
        let mut bytes = vec![0; 8];
        push_tag(
            &mut bytes,
            TAG_MEMORY_MAP,
            &mmap_payload(&[(0, 0x9_f000, 2), (0x10_0000, 0x30_0000, 1)]),
        );
        finish(bytes)
    }

    fn parse(bytes: &[u8]) -> Result<BootInfoV1, BootInfoError> {
        parse_multiboot2(MULTIBOOT2_BOOTLOADER_MAGIC, 0x8000, bytes, &context())
    }

    #[test]
    fn parses_and_reserves_kernel_modules_and_framebuffer() {
        let mut bytes = vec![0; 8];
        push_tag(&mut bytes, TAG_COMMAND_LINE, b"mode=conformance\0");
        push_tag(&mut bytes, TAG_BOOTLOADER_NAME, b"fixture-loader\0");

        let mut module = Vec::new();
        module.extend_from_slice(&0x18_0000u32.to_le_bytes());
        module.extend_from_slice(&0x19_0000u32.to_le_bytes());
        module.extend_from_slice(b"initrd\0");
        push_tag(&mut bytes, TAG_MODULE, &module);

        push_tag(
            &mut bytes,
            TAG_MEMORY_MAP,
            &mmap_payload(&[
                (0, 0x9_f000, 2),
                (0x10_0000, 0x30_0000, 1),
                (0x40_0000, 0x10_0000, 3),
                (0x50_0000, 0x10_0000, 4),
                (0x60_0000, 0x10_0000, 5),
            ]),
        );

        let mut framebuffer = Vec::new();
        framebuffer.extend_from_slice(&0x20_0000u64.to_le_bytes());
        framebuffer.extend_from_slice(&320u32.to_le_bytes());
        framebuffer.extend_from_slice(&80u32.to_le_bytes());
        framebuffer.extend_from_slice(&25u32.to_le_bytes());
        framebuffer.extend_from_slice(&[32, 1, 0, 0]);
        framebuffer.extend_from_slice(&[16, 8, 8, 8, 0, 8]);
        push_tag(&mut bytes, TAG_FRAMEBUFFER, &framebuffer);

        let info = parse(&finish(bytes)).unwrap();
        assert_eq!(info.schema_version, BOOT_INFO_SCHEMA_V1);
        assert_eq!(info.protocol, BootProtocolV1::Multiboot2);
        assert_eq!(info.firmware, FirmwareProtocolV1::Bios);
        assert_eq!(info.command_line.as_deref(), Some("mode=conformance"));
        assert_eq!(info.bootloader_name.as_deref(), Some("fixture-loader"));
        assert_eq!(info.modules.len(), 1);
        assert_eq!(info.modules[0].command_line, "initrd");
        assert!(info.memory_regions.iter().any(|region| {
            region.kind == MemoryRegionKindV1::Reserved
                && region.range.start <= 0x10_0000
                && region.range.end_exclusive >= 0x12_0000
        }));
        assert!(info.memory_regions.iter().any(|region| {
            region.kind == MemoryRegionKindV1::Reserved
                && region.range.start <= 0x18_0000
                && region.range.end_exclusive >= 0x19_0000
        }));
        assert!(info.memory_regions.iter().any(|region| {
            region.kind == MemoryRegionKindV1::Reserved
                && region.range.start <= 0x20_0000
                && region.range.end_exclusive >= 0x20_1f40
        }));
        assert_eq!(info.artifact_digests[0].name, "okernel");
    }

    #[test]
    fn classifies_all_memory_kinds_and_sorts_entries() {
        let mut bytes = vec![0; 8];
        push_tag(
            &mut bytes,
            TAG_MEMORY_MAP,
            &mmap_payload(&[
                (0x50_0000, 0x10_0000, 99),
                (0x40_0000, 0x10_0000, 5),
                (0x30_0000, 0x10_0000, 4),
                (0x20_0000, 0x10_0000, 3),
                (0x10_0000, 0x10_0000, 1),
            ]),
        );
        let info = parse(&finish(bytes)).unwrap();
        assert!(info
            .memory_regions
            .windows(2)
            .all(|pair| pair[0].range.end_exclusive <= pair[1].range.start));
        assert!(info
            .memory_regions
            .iter()
            .any(|region| region.kind == MemoryRegionKindV1::AcpiReclaimable));
        assert!(info
            .memory_regions
            .iter()
            .any(|region| region.kind == MemoryRegionKindV1::AcpiNvs));
        assert!(info
            .memory_regions
            .iter()
            .any(|region| region.kind == MemoryRegionKindV1::BadMemory));
    }

    #[test]
    fn infers_uefi_and_rejects_mixed_pointer_widths() {
        let mut bytes = minimal_information();
        bytes.truncate(bytes.len() - 8);
        let mut pointer = Vec::new();
        pointer.extend_from_slice(&0x1234_5678_9abc_def0u64.to_le_bytes());
        push_tag(&mut bytes, TAG_EFI64_SYSTEM_TABLE, &pointer);
        let info = parse(&finish(bytes)).unwrap();
        assert_eq!(info.firmware, FirmwareProtocolV1::Uefi);

        let mut bytes = minimal_information();
        bytes.truncate(bytes.len() - 8);
        push_tag(&mut bytes, TAG_EFI32_SYSTEM_TABLE, &1u32.to_le_bytes());
        push_tag(&mut bytes, TAG_EFI64_SYSTEM_TABLE, &1u64.to_le_bytes());
        assert_eq!(
            parse(&finish(bytes)).unwrap_err(),
            BootInfoError::ConflictingFirmwareEvidence
        );
    }

    #[test]
    fn accepts_matching_old_and_new_acpi_and_prefers_new() {
        let mut rsdp = [0u8; 36];
        rsdp[..8].copy_from_slice(b"RSD PTR ");
        rsdp[9..15].copy_from_slice(b"OSTADX");
        rsdp[15] = 2;
        rsdp[20..24].copy_from_slice(&36u32.to_le_bytes());
        rsdp[8] = 0u8.wrapping_sub(checksum(&rsdp[..20]));
        rsdp[32] = 0u8.wrapping_sub(checksum(&rsdp));

        let mut bytes = minimal_information();
        bytes.truncate(bytes.len() - 8);
        push_tag(&mut bytes, TAG_ACPI_OLD, &rsdp[..20]);
        push_tag(&mut bytes, TAG_ACPI_NEW, &rsdp);
        let info = parse(&finish(bytes)).unwrap();
        assert_eq!(info.acpi.unwrap().kind, AcpiRsdpKindV1::V2OrLater);
    }

    #[test]
    fn rejects_bad_acpi_checksum_and_conflicting_roots() {
        let mut old = [0u8; 20];
        old[..8].copy_from_slice(b"RSD PTR ");
        old[9..15].copy_from_slice(b"OSTADX");
        old[8] = 0u8.wrapping_sub(checksum(&old));
        let mut bad = old;
        bad[19] ^= 1;

        let mut bytes = minimal_information();
        bytes.truncate(bytes.len() - 8);
        push_tag(&mut bytes, TAG_ACPI_OLD, &bad);
        assert!(matches!(
            parse(&finish(bytes)),
            Err(BootInfoError::InvalidAcpi(_))
        ));

        let mut new = [0u8; 36];
        new[..20].copy_from_slice(&old);
        new[15] = 2;
        new[20..24].copy_from_slice(&36u32.to_le_bytes());
        new[8] = 0;
        new[8] = 0u8.wrapping_sub(checksum(&new[..20]));
        new[32] = 0u8.wrapping_sub(checksum(&new));
        let mut different_old = old;
        different_old[9] ^= 1;
        different_old[8] = 0;
        different_old[8] = 0u8.wrapping_sub(checksum(&different_old));
        let mut bytes = minimal_information();
        bytes.truncate(bytes.len() - 8);
        push_tag(&mut bytes, TAG_ACPI_OLD, &different_old);
        push_tag(&mut bytes, TAG_ACPI_NEW, &new);
        assert_eq!(
            parse(&finish(bytes)).unwrap_err(),
            BootInfoError::ConflictingAcpiTags
        );
    }

    #[test]
    fn validates_indexed_and_text_framebuffers() {
        let mut indexed = Vec::new();
        indexed.extend_from_slice(&0x30_0000u64.to_le_bytes());
        indexed.extend_from_slice(&8u32.to_le_bytes());
        indexed.extend_from_slice(&8u32.to_le_bytes());
        indexed.extend_from_slice(&8u32.to_le_bytes());
        indexed.extend_from_slice(&[8, 0, 0, 0]);
        indexed.extend_from_slice(&2u16.to_le_bytes());
        indexed.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
        let mut bytes = minimal_information();
        bytes.truncate(bytes.len() - 8);
        push_tag(&mut bytes, TAG_FRAMEBUFFER, &indexed);
        assert!(matches!(
            parse(&finish(bytes)).unwrap().framebuffer.unwrap().format,
            FramebufferFormatV1::Indexed { .. }
        ));

        let mut text = Vec::new();
        text.extend_from_slice(&0xb_8000u64.to_le_bytes());
        text.extend_from_slice(&160u32.to_le_bytes());
        text.extend_from_slice(&80u32.to_le_bytes());
        text.extend_from_slice(&25u32.to_le_bytes());
        text.extend_from_slice(&[16, 2, 0, 0]);
        let mut bytes = minimal_information();
        bytes.truncate(bytes.len() - 8);
        push_tag(&mut bytes, TAG_FRAMEBUFFER, &text);
        assert_eq!(
            parse(&finish(bytes)).unwrap().framebuffer.unwrap().format,
            FramebufferFormatV1::EgaText
        );
    }

    #[test]
    fn rejects_bad_magic_alignment_and_information_length() {
        let bytes = minimal_information();
        assert!(matches!(
            parse_multiboot2(0, 0x8000, &bytes, &context()),
            Err(BootInfoError::InvalidMagic { .. })
        ));
        assert!(matches!(
            parse_multiboot2(MULTIBOOT2_BOOTLOADER_MAGIC, 0x8001, &bytes, &context()),
            Err(BootInfoError::MisalignedInformationAddress { .. })
        ));
        let mut wrong = bytes;
        wrong[..4].copy_from_slice(&16u32.to_le_bytes());
        assert!(matches!(
            parse(&wrong),
            Err(BootInfoError::TotalSizeMismatch { .. })
        ));
    }

    #[test]
    fn rejects_missing_or_nonterminal_end_tag() {
        let mut missing = minimal_information();
        missing.truncate(missing.len() - 8);
        let total = u32::try_from(missing.len()).unwrap();
        missing[..4].copy_from_slice(&total.to_le_bytes());
        assert_eq!(parse(&missing).unwrap_err(), BootInfoError::MissingEndTag);

        let mut early = vec![0; 8];
        push_tag(&mut early, TAG_END, &[]);
        push_tag(&mut early, 0x8000_0000, &[]);
        let total = u32::try_from(early.len()).unwrap();
        early[..4].copy_from_slice(&total.to_le_bytes());
        assert_eq!(
            parse(&early).unwrap_err(),
            BootInfoError::TrailingDataAfterEndTag
        );
    }

    #[test]
    fn rejects_small_and_out_of_bounds_tags() {
        let mut small = vec![0; 8];
        small.extend_from_slice(&42u32.to_le_bytes());
        small.extend_from_slice(&4u32.to_le_bytes());
        let total = u32::try_from(small.len()).unwrap();
        small[..4].copy_from_slice(&total.to_le_bytes());
        assert!(matches!(
            parse(&small),
            Err(BootInfoError::InvalidTagSize { .. })
        ));

        let mut outside = vec![0; 16];
        outside[8..12].copy_from_slice(&42u32.to_le_bytes());
        outside[12..16].copy_from_slice(&24u32.to_le_bytes());
        outside[..4].copy_from_slice(&16u32.to_le_bytes());
        assert!(matches!(
            parse(&outside),
            Err(BootInfoError::TagOutOfBounds { .. })
        ));
    }

    #[test]
    fn rejects_duplicate_singletons_but_accepts_multiple_modules() {
        let mut duplicate = minimal_information();
        duplicate.truncate(duplicate.len() - 8);
        push_tag(&mut duplicate, TAG_COMMAND_LINE, b"one\0");
        push_tag(&mut duplicate, TAG_COMMAND_LINE, b"two\0");
        assert_eq!(
            parse(&finish(duplicate)).unwrap_err(),
            BootInfoError::DuplicateTag {
                tag_type: TAG_COMMAND_LINE
            }
        );

        let mut modules = minimal_information();
        modules.truncate(modules.len() - 8);
        for (start, end, name) in [
            (0x20_0000u32, 0x21_0000u32, b"one\0".as_slice()),
            (0x22_0000u32, 0x23_0000u32, b"two\0".as_slice()),
        ] {
            let mut module = Vec::new();
            module.extend_from_slice(&start.to_le_bytes());
            module.extend_from_slice(&end.to_le_bytes());
            module.extend_from_slice(name);
            push_tag(&mut modules, TAG_MODULE, &module);
        }
        assert_eq!(parse(&finish(modules)).unwrap().modules.len(), 2);
    }

    #[test]
    fn rejects_noncanonical_strings() {
        for value in [
            b"unterminated".as_slice(),
            b"early\0tail".as_slice(),
            &[0xff, 0],
        ] {
            let mut bytes = minimal_information();
            bytes.truncate(bytes.len() - 8);
            push_tag(&mut bytes, TAG_COMMAND_LINE, value);
            assert!(matches!(
                parse(&finish(bytes)),
                Err(BootInfoError::InvalidString { .. })
            ));
        }
    }

    #[test]
    fn rejects_memory_map_shape_overflow_and_overlap() {
        let mut bad_size = vec![0; 8];
        let mut payload = mmap_payload(&[(0x10_0000, 0x20_0000, 1)]);
        payload[..4].copy_from_slice(&25u32.to_le_bytes());
        push_tag(&mut bad_size, TAG_MEMORY_MAP, &payload);
        assert!(matches!(
            parse(&finish(bad_size)),
            Err(BootInfoError::MalformedTag {
                tag_type: TAG_MEMORY_MAP,
                ..
            })
        ));

        let mut overflow = vec![0; 8];
        push_tag(
            &mut overflow,
            TAG_MEMORY_MAP,
            &mmap_payload(&[(u64::MAX - 1, 4, 1)]),
        );
        assert!(matches!(
            parse(&finish(overflow)),
            Err(BootInfoError::PhysicalRangeOverflow { .. })
        ));

        let mut overlap = vec![0; 8];
        push_tag(
            &mut overlap,
            TAG_MEMORY_MAP,
            &mmap_payload(&[(0x10_0000, 0x20_0000, 1), (0x20_0000, 0x20_0000, 2)]),
        );
        assert_eq!(
            parse(&finish(overlap)).unwrap_err(),
            BootInfoError::OverlappingMemoryRegions
        );
    }

    #[test]
    fn rejects_overlapping_or_uncovered_loaded_ranges() {
        let mut bytes = minimal_information();
        bytes.truncate(bytes.len() - 8);
        let mut module = Vec::new();
        module.extend_from_slice(&0x11_0000u32.to_le_bytes());
        module.extend_from_slice(&0x13_0000u32.to_le_bytes());
        module.extend_from_slice(b"overlap\0");
        push_tag(&mut bytes, TAG_MODULE, &module);
        assert_eq!(
            parse(&finish(bytes)).unwrap_err(),
            BootInfoError::OverlappingLoadedRanges
        );

        let mut context = context();
        context.kernel = PhysicalRangeV1 {
            start: 0x80_0000,
            end_exclusive: 0x81_0000,
        };
        assert_eq!(
            parse_multiboot2(
                MULTIBOOT2_BOOTLOADER_MAGIC,
                0x8000,
                &minimal_information(),
                &context
            )
            .unwrap_err(),
            BootInfoError::UncoveredLoadedRange { field: "kernel" }
        );
    }

    #[test]
    fn rejects_invalid_context_authority_facts() {
        let bytes = minimal_information();
        let mut bad_arch = context();
        bad_arch.boot_cpu.architecture = BootCpuArchitectureV1::Aarch64;
        assert_eq!(
            parse_multiboot2(MULTIBOOT2_BOOTLOADER_MAGIC, 0x8000, &bytes, &bad_arch).unwrap_err(),
            BootInfoError::InvalidBootCpuArchitecture
        );

        let mut no_digest = context();
        no_digest.artifact_digests.clear();
        assert_eq!(
            parse_multiboot2(MULTIBOOT2_BOOTLOADER_MAGIC, 0x8000, &bytes, &no_digest).unwrap_err(),
            BootInfoError::MissingArtifactDigest
        );

        let mut duplicate = context();
        duplicate
            .artifact_digests
            .push(duplicate.artifact_digests[0].clone());
        assert!(matches!(
            parse_multiboot2(MULTIBOOT2_BOOTLOADER_MAGIC, 0x8000, &bytes, &duplicate),
            Err(BootInfoError::DuplicateArtifactName { .. })
        ));

        let mut bad_serial = context();
        bad_serial.serial.as_mut().unwrap().base_address = 0;
        assert!(matches!(
            parse_multiboot2(MULTIBOOT2_BOOTLOADER_MAGIC, 0x8000, &bytes, &bad_serial),
            Err(BootInfoError::InvalidSerial(_))
        ));
    }

    #[test]
    fn every_truncated_prefix_is_rejected_without_panicking() {
        let bytes = minimal_information();
        for end in 0..bytes.len() {
            let result = std::panic::catch_unwind(|| parse(&bytes[..end]));
            assert!(result.is_ok(), "parser panicked at prefix length {end}");
            assert!(result.unwrap().is_err(), "prefix length {end} was accepted");
        }
    }

    #[test]
    fn bounded_arbitrary_inputs_do_not_panic() {
        let mut state = 0x4f53_5441_4449_5801u64;
        for length in 0..512usize {
            let mut bytes = Vec::with_capacity(length);
            for _ in 0..length {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                bytes.push(state as u8);
            }
            let result = std::panic::catch_unwind(|| parse(&bytes));
            assert!(result.is_ok(), "parser panicked for length {length}");
        }
    }
}
