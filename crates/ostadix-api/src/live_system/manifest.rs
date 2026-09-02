//! Strict `ocore.package/v1` manifests and deterministic package identities.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// The only package-manifest schema accepted by this foundation.
pub const PACKAGE_SCHEMA_V1: &str = "ocore.package/v1";
/// The digest algorithm used for payload and package identities.
pub const SHA256_ALGORITHM: &str = "sha256";

pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_SERVICES: usize = 32;
pub const MAX_CAPABILITY_REQUESTS: usize = 64;
pub const MAX_RIGHTS_PER_REQUEST: usize = 16;
pub const MAX_LOGICAL_NAME_BYTES: usize = 128;
pub const MAX_IDENTIFIER_BYTES: usize = 64;
pub const MAX_PROTOCOL_BYTES: usize = 256;
pub const MAX_PURPOSE_BYTES: usize = 512;
pub const MAX_RUNTIME_ENTRY_BYTES: usize = 4096;
pub const MAX_PAYLOAD_FILES: usize = 4096;
pub const MAX_PAYLOAD_ENTRIES: usize = 8192;
pub const MAX_PAYLOAD_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_PAYLOAD_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_PAYLOAD_PATH_COMPONENTS: usize = 32;
pub const MAX_PAYLOAD_PATH_COMPONENT_BYTES: usize = 255;

const PAYLOAD_DOMAIN: &[u8] = b"ocore.payload-tree/v1\0";
const PACKAGE_DOMAIN: &[u8] = b"ocore.package-object/v1\0";

/// A strict v1 package manifest.
///
/// Every field is required, including the service and capability-request
/// arrays. Empty arrays are represented explicitly in TOML as `services = []`
/// and `capability_requests = []` when a package has neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub schema: String,
    pub name: String,
    pub version: String,
    pub architecture: String,
    pub payload_sha256: String,
    pub runtime: RuntimeManifest,
    pub services: Vec<ServiceManifest>,
    pub capability_requests: Vec<CapabilityRequestManifest>,
    pub health: HealthManifest,
    pub build: BuildManifest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeManifest {
    pub kind: String,
    pub entry: String,
    pub abi: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceManifest {
    pub name: String,
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRequestManifest {
    pub kind: String,
    pub rights: Vec<String>,
    pub purpose: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthManifest {
    pub protocol: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildManifest {
    pub source_sha256: String,
    pub builder: String,
}

/// A validated SHA-256 package identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageDigest(String);

impl PackageDigest {
    /// Parse a bare, lowercase, 64-character SHA-256 digest.
    pub fn from_hex(value: &str) -> Result<Self, PackageError> {
        validate_sha256("package digest", value)?;
        Ok(Self(value.to_owned()))
    }

    /// The bare lowercase hexadecimal digest.
    pub fn as_hex(&self) -> &str {
        &self.0
    }

    /// The explicitly named digest algorithm.
    pub fn algorithm(&self) -> &'static str {
        SHA256_ALGORITHM
    }

    fn from_hasher(hasher: Sha256) -> Self {
        Self(hex::encode(hasher.finalize()))
    }
}

impl std::fmt::Display for PackageDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}", self.algorithm(), self.as_hex())
    }
}

/// One immutable regular file captured from a verified payload tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadFile {
    path: String,
    executable: bool,
    contents: Vec<u8>,
}

impl PayloadFile {
    /// Portable, slash-separated path relative to the payload root.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Whether at least one executable bit was present on Unix at ingestion.
    pub fn is_executable(&self) -> bool {
        self.executable
    }

    /// The bytes that participated in verification and package identity.
    pub fn contents(&self) -> &[u8] {
        &self.contents
    }
}

/// A parsed manifest and the exact payload bytes it authenticates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPackage {
    manifest: PackageManifest,
    digest: PackageDigest,
    payload_files: Vec<PayloadFile>,
}

impl VerifiedPackage {
    /// Parse and validate a manifest, capture the complete regular-file tree,
    /// verify `payload_sha256`, and derive the package's content identity.
    pub fn load(manifest_toml: &str, payload_root: &Path) -> Result<Self, PackageError> {
        let manifest = PackageManifest::parse_toml(manifest_toml)?;
        let payload_files = scan_payload_tree(payload_root)?;
        let actual_payload = payload_digest(&payload_files);
        if actual_payload != manifest.payload_sha256 {
            return Err(PackageError::PayloadDigestMismatch {
                declared: manifest.payload_sha256,
                actual: actual_payload,
            });
        }

        let digest = package_digest(&manifest, &payload_files)?;
        Ok(Self {
            manifest,
            digest,
            payload_files,
        })
    }

    pub fn manifest(&self) -> &PackageManifest {
        &self.manifest
    }

    pub fn digest(&self) -> &PackageDigest {
        &self.digest
    }

    pub fn payload_sha256(&self) -> &str {
        &self.manifest.payload_sha256
    }

    pub fn payload_files(&self) -> &[PayloadFile] {
        &self.payload_files
    }
}

#[derive(Debug, Error)]
pub enum PackageError {
    #[error("invalid package manifest TOML: {0}")]
    InvalidToml(#[from] toml::de::Error),

    #[error("could not encode canonical package manifest TOML: {0}")]
    EncodeToml(#[from] toml::ser::Error),

    #[error("unsupported package schema `{found}`; expected `{PACKAGE_SCHEMA_V1}`")]
    UnsupportedSchema { found: String },

    #[error("invalid manifest field `{field}`: {reason}")]
    InvalidField { field: &'static str, reason: String },

    #[error("{resource} exceeds its limit of {limit} (got {actual})")]
    LimitExceeded {
        resource: &'static str,
        limit: u64,
        actual: u64,
    },

    #[error("invalid SHA-256 in `{field}`: expected 64 lowercase hexadecimal characters")]
    InvalidSha256 { field: &'static str },

    #[error("duplicate value `{value}` in manifest field `{field}`")]
    Duplicate { field: &'static str, value: String },

    #[error("payload root is not a directory: {path:?}")]
    PayloadRootNotDirectory { path: PathBuf },

    #[error("symbolic links are forbidden in package payloads: {path:?}")]
    Symlink { path: PathBuf },

    #[error("only directories and regular files are allowed in package payloads: {path:?}")]
    UnsupportedFileType { path: PathBuf },

    #[error("payload path is not valid UTF-8: {path:?}")]
    NonUtf8Path { path: PathBuf },

    #[error("invalid payload path {path:?}: {reason}")]
    InvalidPayloadPath { path: PathBuf, reason: String },

    #[error("duplicate normalized payload path `{path}`")]
    DuplicatePayloadPath { path: String },

    #[error("payload file changed while it was being captured: {path:?}")]
    PayloadChanged { path: PathBuf },

    #[error("payload file {path:?} exceeds the {max}-byte limit (got {size})")]
    PayloadFileTooLarge { path: PathBuf, size: u64, max: u64 },

    #[error("payload tree exceeds the {max}-byte limit (got at least {size})")]
    PayloadTreeTooLarge { size: u64, max: u64 },

    #[error("could not {operation} payload path {path:?}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("payload SHA-256 mismatch: manifest declares {declared}, computed {actual}")]
    PayloadDigestMismatch { declared: String, actual: String },
}

impl PackageManifest {
    /// Parse strict TOML and perform semantic validation.
    pub fn parse_toml(input: &str) -> Result<Self, PackageError> {
        enforce_limit(
            "manifest bytes",
            input.len() as u64,
            MAX_MANIFEST_BYTES as u64,
        )?;
        let manifest: Self = toml::from_str(input)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate schema, digests, names, paths, and duplicate declarations.
    pub fn validate(&self) -> Result<(), PackageError> {
        if self.schema != PACKAGE_SCHEMA_V1 {
            return Err(PackageError::UnsupportedSchema {
                found: self.schema.clone(),
            });
        }

        validate_logical_name("name", &self.name)?;
        semver::Version::parse(&self.version).map_err(|error| PackageError::InvalidField {
            field: "version",
            reason: error.to_string(),
        })?;
        validate_identifier("architecture", &self.architecture)?;
        validate_sha256("payload_sha256", &self.payload_sha256)?;

        validate_identifier("runtime.kind", &self.runtime.kind)?;
        validate_runtime_entry(&self.runtime.entry)?;
        validate_bounded_text("runtime.abi", &self.runtime.abi, MAX_PROTOCOL_BYTES)?;

        enforce_limit(
            "service declarations",
            self.services.len() as u64,
            MAX_SERVICES as u64,
        )?;

        let mut service_names = BTreeSet::new();
        for service in &self.services {
            validate_identifier("services.name", &service.name)?;
            validate_bounded_text("services.protocol", &service.protocol, MAX_PROTOCOL_BYTES)?;
            if !service_names.insert(service.name.as_str()) {
                return Err(PackageError::Duplicate {
                    field: "services.name",
                    value: service.name.clone(),
                });
            }
        }

        enforce_limit(
            "capability request declarations",
            self.capability_requests.len() as u64,
            MAX_CAPABILITY_REQUESTS as u64,
        )?;
        let mut requests = BTreeSet::new();
        for request in &self.capability_requests {
            validate_identifier("capability_requests.kind", &request.kind)?;
            validate_bounded_text(
                "capability_requests.purpose",
                &request.purpose,
                MAX_PURPOSE_BYTES,
            )?;
            if request.rights.is_empty() {
                return Err(PackageError::InvalidField {
                    field: "capability_requests.rights",
                    reason: "must contain at least one right".to_owned(),
                });
            }

            enforce_limit(
                "rights per capability request",
                request.rights.len() as u64,
                MAX_RIGHTS_PER_REQUEST as u64,
            )?;

            let mut rights = BTreeSet::new();
            for right in &request.rights {
                validate_identifier("capability_requests.rights", right)?;
                if !rights.insert(right.as_str()) {
                    return Err(PackageError::Duplicate {
                        field: "capability_requests.rights",
                        value: right.clone(),
                    });
                }
            }

            if !requests.insert((request.kind.as_str(), request.purpose.as_str())) {
                return Err(PackageError::Duplicate {
                    field: "capability_requests",
                    value: format!("{}:{}", request.kind, request.purpose),
                });
            }
        }

        validate_bounded_text("health.protocol", &self.health.protocol, MAX_PROTOCOL_BYTES)?;
        if !(1..=60_000).contains(&self.health.timeout_ms) {
            return Err(PackageError::InvalidField {
                field: "health.timeout_ms",
                reason: "must be between 1 and 60000 milliseconds".to_owned(),
            });
        }

        validate_sha256("build.source_sha256", &self.build.source_sha256)?;
        validate_bounded_text("build.builder", &self.build.builder, MAX_PROTOCOL_BYTES)?;
        Ok(())
    }

    /// A stable binary encoding of parsed manifest semantics.
    ///
    /// Declaration order for services, requests, and rights is not semantic;
    /// those collections are sorted here. Duplicate values are rejected by
    /// [`Self::validate`] rather than silently collapsed.
    pub fn canonical_identity_bytes(&self) -> Result<Vec<u8>, PackageError> {
        self.validate()?;
        let manifest = self.canonicalized();
        let mut output = Vec::new();
        output.extend_from_slice(b"ocore.package-manifest/v1\0");
        encode_string(&mut output, &manifest.schema);
        encode_string(&mut output, &manifest.name);
        encode_string(&mut output, &manifest.version);
        encode_string(&mut output, &manifest.architecture);
        encode_string(&mut output, &manifest.payload_sha256);

        encode_string(&mut output, &manifest.runtime.kind);
        encode_string(&mut output, &manifest.runtime.entry);
        encode_string(&mut output, &manifest.runtime.abi);

        encode_u64(&mut output, manifest.services.len() as u64);
        for service in &manifest.services {
            encode_string(&mut output, &service.name);
            encode_string(&mut output, &service.protocol);
        }

        encode_u64(&mut output, manifest.capability_requests.len() as u64);
        for request in &manifest.capability_requests {
            encode_string(&mut output, &request.kind);
            encode_u64(&mut output, request.rights.len() as u64);
            for right in &request.rights {
                encode_string(&mut output, right);
            }
            encode_string(&mut output, &request.purpose);
        }

        encode_string(&mut output, &manifest.health.protocol);
        encode_u64(&mut output, manifest.health.timeout_ms);
        encode_string(&mut output, &manifest.build.source_sha256);
        encode_string(&mut output, &manifest.build.builder);
        Ok(output)
    }

    /// Deterministic TOML used for the stored, human-readable manifest copy.
    /// Package identity is based on [`Self::canonical_identity_bytes`], not on
    /// serializer formatting.
    pub fn canonical_toml(&self) -> Result<String, PackageError> {
        self.validate()?;
        let encoded = toml::to_string(&self.canonicalized())?;
        enforce_limit(
            "canonical manifest bytes",
            encoded.len() as u64,
            MAX_MANIFEST_BYTES as u64,
        )?;
        Ok(encoded)
    }

    fn canonicalized(&self) -> Self {
        let mut manifest = self.clone();
        manifest.services.sort_by(|left, right| {
            (&left.name, &left.protocol).cmp(&(&right.name, &right.protocol))
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
        manifest
    }
}

/// Compute the deterministic SHA-256 of a complete regular-file payload tree.
pub fn payload_sha256(payload_root: &Path) -> Result<String, PackageError> {
    Ok(payload_digest(&scan_payload_tree(payload_root)?))
}

pub(crate) fn validate_logical_name(field: &'static str, value: &str) -> Result<(), PackageError> {
    enforce_limit(field, value.len() as u64, MAX_LOGICAL_NAME_BYTES as u64)?;
    if value.is_empty() || value.starts_with('/') || value.ends_with('/') {
        return Err(PackageError::InvalidField {
            field,
            reason: "must be a non-empty relative logical name".to_owned(),
        });
    }
    for component in value.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(PackageError::InvalidField {
                field,
                reason: "contains an empty, current-directory, or parent-directory component"
                    .to_owned(),
            });
        }
        if !component.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | '+' | '@')
        }) {
            return Err(PackageError::InvalidField {
                field,
                reason: format!("contains unsupported component `{component}`"),
            });
        }
    }
    Ok(())
}

pub(crate) fn normalize_relative_payload_path(path: &Path) -> Result<String, PackageError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(PackageError::InvalidPayloadPath {
            path: path.to_path_buf(),
            reason: "must be a non-empty relative path".to_owned(),
        });
    }
    let raw = path.to_str().ok_or_else(|| PackageError::NonUtf8Path {
        path: path.to_path_buf(),
    })?;
    if raw.contains('\\') || raw.contains('\0') {
        return Err(PackageError::InvalidPayloadPath {
            path: path.to_path_buf(),
            reason: "backslashes and NUL bytes are forbidden".to_owned(),
        });
    }
    enforce_limit(
        "payload path bytes",
        raw.len() as u64,
        MAX_RUNTIME_ENTRY_BYTES as u64,
    )?;
    let components: Vec<_> = raw.split('/').collect();
    enforce_limit(
        "payload path components",
        components.len() as u64,
        MAX_PAYLOAD_PATH_COMPONENTS as u64,
    )?;
    for component in components {
        if component.is_empty() || component == "." || component == ".." {
            return Err(PackageError::InvalidPayloadPath {
                path: path.to_path_buf(),
                reason: "contains an empty, current-directory, or parent-directory component"
                    .to_owned(),
            });
        }
        enforce_limit(
            "payload path component bytes",
            component.len() as u64,
            MAX_PAYLOAD_PATH_COMPONENT_BYTES as u64,
        )?;
    }
    Ok(raw.to_owned())
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), PackageError> {
    enforce_limit(field, value.len() as u64, MAX_IDENTIFIER_BYTES as u64)?;
    if value.is_empty()
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | '+')
        })
    {
        return Err(PackageError::InvalidField {
            field,
            reason: "must contain only ASCII letters, digits, `.`, `_`, `-`, or `+`".to_owned(),
        });
    }
    Ok(())
}

fn validate_bounded_text(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), PackageError> {
    enforce_limit(field, value.len() as u64, max_bytes as u64)?;
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(PackageError::InvalidField {
            field,
            reason: "must be non-empty and contain no control characters".to_owned(),
        });
    }
    Ok(())
}

fn validate_runtime_entry(value: &str) -> Result<(), PackageError> {
    runtime_entry_payload_path(value).map(|_| ())
}

/// Map a package-internal absolute runtime entry such as `/bin/live` to its
/// relative payload-tree path (`bin/live`). No filesystem access occurs.
pub fn runtime_entry_payload_path(value: &str) -> Result<PathBuf, PackageError> {
    enforce_limit(
        "runtime.entry bytes",
        value.len() as u64,
        MAX_RUNTIME_ENTRY_BYTES as u64,
    )?;
    if !value.starts_with('/') || value == "/" || value.contains('\\') || value.contains('\0') {
        return Err(PackageError::InvalidField {
            field: "runtime.entry",
            reason: "must be an absolute payload path".to_owned(),
        });
    }
    normalize_relative_payload_path(Path::new(&value[1..]))
        .map(PathBuf::from)
        .map_err(|error| PackageError::InvalidField {
            field: "runtime.entry",
            reason: error.to_string(),
        })
}

fn enforce_limit(resource: &'static str, actual: u64, limit: u64) -> Result<(), PackageError> {
    if actual > limit {
        return Err(PackageError::LimitExceeded {
            resource,
            limit,
            actual,
        });
    }
    Ok(())
}

fn validate_sha256(field: &'static str, value: &str) -> Result<(), PackageError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(PackageError::InvalidSha256 { field });
    }
    Ok(())
}

fn scan_payload_tree(payload_root: &Path) -> Result<Vec<PayloadFile>, PackageError> {
    let root = open_payload_directory_path(payload_root)?;

    // Preflight the complete tree before reading file contents, but do not keep
    // one descriptor open per regular file. Retaining every descriptor makes
    // the declared 4096-file limit unreachable on hosts whose RLIMIT_NOFILE is
    // lower than that limit. After preflight succeeds, each candidate is
    // reopened through the still-open payload-root descriptor, checked against
    // the object observed during preflight, and captured immediately.
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    let mut total_bytes = 0_u64;
    let mut total_entries = 0_usize;
    scan_directory(
        payload_root,
        Path::new(""),
        &root,
        &mut candidates,
        &mut seen,
        &mut total_bytes,
        &mut total_entries,
    )?;
    candidates.sort_by(|left, right| left.normalized.cmp(&right.normalized));

    let mut payload_files = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        payload_files.push(capture_payload_file(payload_root, &root, candidate)?);
    }
    Ok(payload_files)
}

#[derive(Debug)]
struct PayloadCandidate {
    normalized: String,
    relative: PathBuf,
    path: PathBuf,
    expected_size: u64,
    #[cfg(unix)]
    expected_device: u64,
    #[cfg(unix)]
    expected_inode: u64,
}

#[cfg(unix)]
fn scan_directory(
    payload_root: &Path,
    relative_directory: &Path,
    directory: &File,
    candidates: &mut Vec<PayloadCandidate>,
    seen: &mut BTreeSet<String>,
    total_bytes: &mut u64,
    total_entries: &mut usize,
) -> Result<(), PackageError> {
    let directory_path = payload_root.join(relative_directory);
    let mut children = Vec::new();
    for name in directory_entry_names(directory, &directory_path)? {
        *total_entries = total_entries.saturating_add(1);
        enforce_limit(
            "payload directory entries",
            *total_entries as u64,
            MAX_PAYLOAD_ENTRIES as u64,
        )?;
        let relative = relative_directory.join(&name);
        let normalized = normalize_relative_payload_path(&relative)?;
        children.push((normalized, relative, name));
    }
    children.sort_by(|left, right| left.0.cmp(&right.0));

    for (normalized, relative, name) in children {
        let path = payload_root.join(&relative);
        let file = open_child_no_follow(directory, &name, &path)?;
        let metadata = file.metadata().map_err(|source| PackageError::Io {
            operation: "inspect opened entry",
            path: path.clone(),
            source,
        })?;
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            scan_directory(
                payload_root,
                &relative,
                &file,
                candidates,
                seen,
                total_bytes,
                total_entries,
            )?;
            continue;
        }
        if !file_type.is_file() {
            return Err(PackageError::UnsupportedFileType { path });
        }
        if !seen.insert(normalized.clone()) {
            return Err(PackageError::DuplicatePayloadPath { path: normalized });
        }
        enforce_limit(
            "payload files",
            (candidates.len() + 1) as u64,
            MAX_PAYLOAD_FILES as u64,
        )?;
        let size = metadata.len();
        if size > MAX_PAYLOAD_FILE_BYTES {
            return Err(PackageError::PayloadFileTooLarge {
                path,
                size,
                max: MAX_PAYLOAD_FILE_BYTES,
            });
        }
        let next_total =
            total_bytes
                .checked_add(size)
                .ok_or(PackageError::PayloadTreeTooLarge {
                    size: u64::MAX,
                    max: MAX_PAYLOAD_BYTES,
                })?;
        if next_total > MAX_PAYLOAD_BYTES {
            return Err(PackageError::PayloadTreeTooLarge {
                size: next_total,
                max: MAX_PAYLOAD_BYTES,
            });
        }
        *total_bytes = next_total;
        let (expected_device, expected_inode) = {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino())
        };
        candidates.push(PayloadCandidate {
            normalized,
            relative,
            path,
            expected_size: size,
            expected_device,
            expected_inode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn scan_directory(
    payload_root: &Path,
    relative_directory: &Path,
    _directory: &File,
    candidates: &mut Vec<PayloadCandidate>,
    seen: &mut BTreeSet<String>,
    total_bytes: &mut u64,
    total_entries: &mut usize,
) -> Result<(), PackageError> {
    // Rust's portable filesystem API has no openat/O_NOFOLLOW equivalent. The
    // fallback checks every component during preflight and checks it again when
    // capturing, but cannot promise Unix's descriptor-relative traversal
    // semantics.
    let directory_path = payload_root.join(relative_directory);
    let entries = fs::read_dir(&directory_path).map_err(|source| PackageError::Io {
        operation: "read directory",
        path: directory_path.clone(),
        source,
    })?;
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| PackageError::Io {
            operation: "read directory entry",
            path: directory_path.clone(),
            source,
        })?;
        *total_entries = total_entries.saturating_add(1);
        enforce_limit(
            "payload directory entries",
            *total_entries as u64,
            MAX_PAYLOAD_ENTRIES as u64,
        )?;
        let relative = relative_directory.join(entry.file_name());
        let normalized = normalize_relative_payload_path(&relative)?;
        children.push((normalized, relative));
    }
    children.sort_by(|left, right| left.0.cmp(&right.0));

    for (normalized, relative) in children {
        let path = payload_root.join(&relative);
        let metadata = fs::symlink_metadata(&path).map_err(|source| PackageError::Io {
            operation: "inspect",
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(PackageError::Symlink { path });
        }
        if metadata.is_dir() {
            let directory = File::open(&path).map_err(|source| PackageError::Io {
                operation: "open directory",
                path: path.clone(),
                source,
            })?;
            scan_directory(
                payload_root,
                &relative,
                &directory,
                candidates,
                seen,
                total_bytes,
                total_entries,
            )?;
            continue;
        }
        if !metadata.is_file() {
            return Err(PackageError::UnsupportedFileType { path });
        }
        if !seen.insert(normalized.clone()) {
            return Err(PackageError::DuplicatePayloadPath { path: normalized });
        }
        enforce_limit(
            "payload files",
            (candidates.len() + 1) as u64,
            MAX_PAYLOAD_FILES as u64,
        )?;
        let size = metadata.len();
        if size > MAX_PAYLOAD_FILE_BYTES {
            return Err(PackageError::PayloadFileTooLarge {
                path,
                size,
                max: MAX_PAYLOAD_FILE_BYTES,
            });
        }
        let next_total =
            total_bytes
                .checked_add(size)
                .ok_or(PackageError::PayloadTreeTooLarge {
                    size: u64::MAX,
                    max: MAX_PAYLOAD_BYTES,
                })?;
        if next_total > MAX_PAYLOAD_BYTES {
            return Err(PackageError::PayloadTreeTooLarge {
                size: next_total,
                max: MAX_PAYLOAD_BYTES,
            });
        }
        *total_bytes = next_total;
        let file = File::open(&path).map_err(|source| PackageError::Io {
            operation: "open",
            path: path.clone(),
            source,
        })?;
        drop(file);
        candidates.push(PayloadCandidate {
            normalized,
            relative,
            path,
            expected_size: size,
        });
    }
    Ok(())
}

fn capture_payload_file(
    payload_root: &Path,
    root: &File,
    candidate: PayloadCandidate,
) -> Result<PayloadFile, PackageError> {
    let file = open_payload_regular_file_from_root(payload_root, root, &candidate.relative)?;
    let opened_metadata = file.metadata().map_err(|source| PackageError::Io {
        operation: "inspect opened file",
        path: candidate.path.clone(),
        source,
    })?;
    if !opened_metadata.is_file() {
        return Err(PackageError::UnsupportedFileType {
            path: candidate.path,
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened_metadata.dev() != candidate.expected_device
            || opened_metadata.ino() != candidate.expected_inode
        {
            return Err(PackageError::PayloadChanged {
                path: candidate.path,
            });
        }
    }
    if opened_metadata.len() > MAX_PAYLOAD_FILE_BYTES {
        return Err(PackageError::PayloadFileTooLarge {
            path: candidate.path,
            size: opened_metadata.len(),
            max: MAX_PAYLOAD_FILE_BYTES,
        });
    }
    if opened_metadata.len() != candidate.expected_size {
        return Err(PackageError::PayloadChanged {
            path: candidate.path,
        });
    }

    let executable = is_executable(&opened_metadata);
    let initial_capacity = usize::try_from(candidate.expected_size)
        .unwrap_or(usize::MAX)
        .min(MAX_PAYLOAD_FILE_BYTES as usize);
    let mut contents = Vec::with_capacity(initial_capacity);
    let mut limited = file.take(MAX_PAYLOAD_FILE_BYTES + 1);
    limited
        .read_to_end(&mut contents)
        .map_err(|source| PackageError::Io {
            operation: "read",
            path: candidate.path.clone(),
            source,
        })?;
    let file = limited.into_inner();
    if contents.len() as u64 > MAX_PAYLOAD_FILE_BYTES {
        return Err(PackageError::PayloadFileTooLarge {
            path: candidate.path,
            size: contents.len() as u64,
            max: MAX_PAYLOAD_FILE_BYTES,
        });
    }
    let closed_metadata = file.metadata().map_err(|source| PackageError::Io {
        operation: "reinspect opened file",
        path: candidate.path.clone(),
        source,
    })?;
    if closed_metadata.len() != candidate.expected_size
        || contents.len() as u64 != candidate.expected_size
    {
        return Err(PackageError::PayloadChanged {
            path: candidate.path,
        });
    }
    Ok(PayloadFile {
        path: candidate.normalized,
        executable,
        contents,
    })
}

/// Open a regular payload file without permitting any path component to be a
/// symlink. On Unix, every lookup is relative to a previously opened directory
/// and the returned descriptor is the authority callers must read.
pub(crate) fn open_payload_regular_file(
    payload_root: &Path,
    relative: &Path,
) -> Result<File, PackageError> {
    let root = open_payload_directory_path(payload_root)?;
    open_payload_regular_file_from_root(payload_root, &root, relative)
}

#[cfg(unix)]
fn open_payload_regular_file_from_root(
    payload_root: &Path,
    root: &File,
    relative: &Path,
) -> Result<File, PackageError> {
    let normalized = normalize_relative_payload_path(relative)?;
    let normalized = Path::new(&normalized);
    let mut directory = root.try_clone().map_err(|source| PackageError::Io {
        operation: "duplicate payload root descriptor",
        path: payload_root.to_path_buf(),
        source,
    })?;
    let components: Vec<_> = normalized.components().collect();
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = component else {
            unreachable!("normalized payload paths contain only normal components");
        };
        let display_path = payload_root.join(components[..=index].iter().fold(
            PathBuf::new(),
            |mut path, component| {
                path.push(component.as_os_str());
                path
            },
        ));
        let opened = if index + 1 == components.len() {
            open_child_no_follow(&directory, name, &display_path)?
        } else {
            open_directory_child_no_follow(&directory, name, &display_path)?
        };
        let metadata = opened.metadata().map_err(|source| PackageError::Io {
            operation: "inspect opened entry",
            path: display_path.clone(),
            source,
        })?;
        if index + 1 == components.len() {
            if !metadata.is_file() {
                return Err(PackageError::UnsupportedFileType { path: display_path });
            }
            return Ok(opened);
        }
        if !metadata.is_dir() {
            return Err(PackageError::UnsupportedFileType { path: display_path });
        }
        directory = opened;
    }
    unreachable!("normalized payload paths are non-empty");
}

#[cfg(not(unix))]
fn open_payload_regular_file_from_root(
    payload_root: &Path,
    _root: &File,
    relative: &Path,
) -> Result<File, PackageError> {
    let normalized = normalize_relative_payload_path(relative)?;
    let normalized = Path::new(&normalized);

    // Best available portable fallback: reject symlinks component by
    // component and keep the opened final file. The standard library does not
    // expose descriptor-relative traversal on non-Unix targets.
    let mut current = payload_root.to_path_buf();
    let component_count = normalized.components().count();
    for (index, component) in normalized.components().enumerate() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current).map_err(|source| PackageError::Io {
            operation: "inspect",
            path: current.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(PackageError::Symlink { path: current });
        }
        let is_last = index + 1 == component_count;
        if (!is_last && !metadata.is_dir()) || (is_last && !metadata.is_file()) {
            return Err(PackageError::UnsupportedFileType { path: current });
        }
    }
    File::open(&current).map_err(|source| PackageError::Io {
        operation: "open",
        path: current,
        source,
    })
}

#[cfg(unix)]
fn open_payload_directory_path(path: &Path) -> Result<File, PackageError> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    if path.as_os_str().is_empty() {
        return Err(PackageError::PayloadRootNotDirectory {
            path: path.to_path_buf(),
        });
    }
    let mut lexical = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        PathBuf::new()
    };
    for component in path.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::Normal(name) => lexical.push(name),
            std::path::Component::ParentDir | std::path::Component::Prefix(_) => {
                return Err(PackageError::InvalidPayloadPath {
                    path: path.to_path_buf(),
                    reason: "payload root parent traversal components are forbidden".to_owned(),
                });
            }
        }
    }
    if lexical.as_os_str().is_empty() {
        lexical.push(".");
    }
    let encoded = CString::new(lexical.as_os_str().as_bytes()).map_err(|_| {
        PackageError::InvalidPayloadPath {
            path: path.to_path_buf(),
            reason: "NUL bytes are forbidden".to_owned(),
        }
    })?;
    let descriptor = unsafe {
        // SAFETY: `encoded` is NUL-terminated and these flags need no mode.
        // O_NOFOLLOW protects the payload-root entry itself; all paths beneath
        // this trusted root are subsequently resolved descriptor-relative.
        libc::open(
            encoded.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        let source = io::Error::last_os_error();
        let leaf_is_symlink = fs::symlink_metadata(&lexical)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false);
        if source.raw_os_error() == Some(libc::ELOOP) || leaf_is_symlink {
            return Err(PackageError::Symlink {
                path: path.to_path_buf(),
            });
        }
        if source.raw_os_error() == Some(libc::ENOTDIR) {
            return Err(PackageError::PayloadRootNotDirectory {
                path: path.to_path_buf(),
            });
        }
        return Err(PackageError::Io {
            operation: "open payload root without following links",
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(unsafe {
        // SAFETY: `descriptor` is newly owned after a successful open.
        File::from_raw_fd(descriptor)
    })
}

#[cfg(not(unix))]
fn open_payload_directory_path(path: &Path) -> Result<File, PackageError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PackageError::Io {
        operation: "inspect",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(PackageError::Symlink {
            path: path.to_path_buf(),
        });
    }
    if !metadata.is_dir() {
        return Err(PackageError::PayloadRootNotDirectory {
            path: path.to_path_buf(),
        });
    }
    File::open(path).map_err(|source| PackageError::Io {
        operation: "open directory",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn open_directory_child_no_follow(
    directory: &File,
    name: &std::ffi::OsStr,
    display_path: &Path,
) -> Result<File, PackageError> {
    preflight_child_type(directory, name, display_path)?;
    openat_no_follow(
        directory,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        display_path,
    )
}

#[cfg(unix)]
fn open_child_no_follow(
    directory: &File,
    name: &std::ffi::OsStr,
    display_path: &Path,
) -> Result<File, PackageError> {
    preflight_child_type(directory, name, display_path)?;
    openat_no_follow(
        directory,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        display_path,
    )
}

#[cfg(unix)]
fn preflight_child_type(
    directory: &File,
    name: &std::ffi::OsStr,
    display_path: &Path,
) -> Result<(), PackageError> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let name = CString::new(name.as_bytes()).map_err(|_| PackageError::InvalidPayloadPath {
        path: display_path.to_path_buf(),
        reason: "NUL bytes are forbidden".to_owned(),
    })?;
    let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        // SAFETY: the directory and C string are valid, and fstatat initializes
        // `status` on success without following the named entry.
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            status.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        return Err(PackageError::Io {
            operation: "inspect entry without following links",
            path: display_path.to_path_buf(),
            source: io::Error::last_os_error(),
        });
    }
    let status = unsafe {
        // SAFETY: fstatat returned success, so `status` is initialized.
        status.assume_init()
    };
    let kind = status.st_mode & libc::S_IFMT;
    if kind == libc::S_IFLNK {
        return Err(PackageError::Symlink {
            path: display_path.to_path_buf(),
        });
    }
    if kind != libc::S_IFDIR && kind != libc::S_IFREG {
        return Err(PackageError::UnsupportedFileType {
            path: display_path.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn openat_no_follow(
    directory: &File,
    name: &std::ffi::OsStr,
    flags: libc::c_int,
    display_path: &Path,
) -> Result<File, PackageError> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    let name = CString::new(name.as_bytes()).map_err(|_| PackageError::InvalidPayloadPath {
        path: display_path.to_path_buf(),
        reason: "NUL bytes are forbidden".to_owned(),
    })?;
    let descriptor = unsafe {
        // SAFETY: the parent descriptor remains open, `name` is NUL-terminated,
        // and these flags do not require a mode argument.
        libc::openat(directory.as_raw_fd(), name.as_ptr(), flags)
    };
    if descriptor < 0 {
        let source = io::Error::last_os_error();
        if source.raw_os_error() == Some(libc::ELOOP) {
            return Err(PackageError::Symlink {
                path: display_path.to_path_buf(),
            });
        }
        return Err(PackageError::Io {
            operation: "open without following links",
            path: display_path.to_path_buf(),
            source,
        });
    }
    Ok(unsafe {
        // SAFETY: `descriptor` is newly owned after a successful openat.
        File::from_raw_fd(descriptor)
    })
}

#[cfg(unix)]
fn directory_entry_names(
    directory: &File,
    display_path: &Path,
) -> Result<Vec<std::ffi::OsString>, PackageError> {
    use std::ffi::CStr;
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::os::unix::ffi::OsStringExt;

    let descriptor = directory
        .try_clone()
        .map_err(|source| PackageError::Io {
            operation: "duplicate directory descriptor",
            path: display_path.to_path_buf(),
            source,
        })?
        .into_raw_fd();
    let stream = unsafe {
        // SAFETY: `descriptor` is newly owned. fdopendir consumes it on success.
        libc::fdopendir(descriptor)
    };
    if stream.is_null() {
        unsafe {
            // SAFETY: fdopendir failed, so ownership remains with us.
            drop(File::from_raw_fd(descriptor));
        }
        return Err(PackageError::Io {
            operation: "open directory stream",
            path: display_path.to_path_buf(),
            source: io::Error::last_os_error(),
        });
    }

    let result = (|| {
        let mut names = Vec::new();
        loop {
            clear_readdir_errno();
            let entry = unsafe {
                // SAFETY: `stream` remains valid until closed below.
                libc::readdir(stream)
            };
            if entry.is_null() {
                if let Some(source) = readdir_error() {
                    return Err(PackageError::Io {
                        operation: "read directory entry",
                        path: display_path.to_path_buf(),
                        source,
                    });
                }
                break;
            }
            let bytes = unsafe {
                // SAFETY: POSIX guarantees d_name is NUL-terminated for a
                // successfully returned directory entry.
                CStr::from_ptr((*entry).d_name.as_ptr())
            }
            .to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            names.push(std::ffi::OsString::from_vec(bytes.to_vec()));
        }
        Ok(names)
    })();
    let close_result = unsafe {
        // SAFETY: `stream` is a live DIR pointer and closed exactly once.
        libc::closedir(stream)
    };
    if close_result != 0 && result.is_ok() {
        return Err(PackageError::Io {
            operation: "close directory stream",
            path: display_path.to_path_buf(),
            source: io::Error::last_os_error(),
        });
    }
    result
}

#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "emscripten",
    target_os = "hurd",
    target_os = "l4re"
))]
fn clear_readdir_errno() {
    unsafe {
        // SAFETY: this writes the calling thread's errno slot.
        *libc::__errno_location() = 0;
    }
}

#[cfg(target_os = "android")]
fn clear_readdir_errno() {
    unsafe {
        // SAFETY: Bionic exposes the calling thread's errno slot through
        // __errno(), unlike glibc's __errno_location().
        *libc::__errno() = 0;
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
    target_os = "freebsd",
))]
fn clear_readdir_errno() {
    unsafe {
        // SAFETY: this writes the calling thread's errno slot.
        *libc::__error() = 0;
    }
}

#[cfg(any(target_os = "openbsd", target_os = "netbsd"))]
fn clear_readdir_errno() {
    unsafe {
        // SAFETY: this writes the calling thread's errno slot.
        *libc::__errno() = 0;
    }
}

#[cfg(any(target_os = "solaris", target_os = "illumos"))]
fn clear_readdir_errno() {
    unsafe {
        // SAFETY: this writes the calling thread's errno slot.
        *libc::___errno() = 0;
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "dragonfly",
    target_os = "emscripten",
    target_os = "hurd",
    target_os = "l4re",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "solaris",
    target_os = "illumos"
)))]
fn clear_readdir_errno() {}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "dragonfly",
    target_os = "emscripten",
    target_os = "hurd",
    target_os = "l4re",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "solaris",
    target_os = "illumos"
))]
fn readdir_error() -> Option<io::Error> {
    let error = io::Error::last_os_error();
    (error.raw_os_error() != Some(0)).then_some(error)
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "dragonfly",
        target_os = "emscripten",
        target_os = "hurd",
        target_os = "l4re",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "visionos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos"
    ))
))]
fn readdir_error() -> Option<io::Error> {
    // libc does not expose one portable errno accessor across all Unix
    // variants. Traversal remains descriptor-relative and fail-closed for
    // open/fstat errors; on these less common targets a terminal readdir null
    // is conservatively treated as end-of-stream.
    None
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

fn payload_digest(payload_files: &[PayloadFile]) -> String {
    let mut hasher = Sha256::new();
    encode_payload_tree(&mut hasher, payload_files);
    hex::encode(hasher.finalize())
}

fn package_digest(
    manifest: &PackageManifest,
    payload_files: &[PayloadFile],
) -> Result<PackageDigest, PackageError> {
    let manifest_bytes = manifest.canonical_identity_bytes()?;
    let mut hasher = Sha256::new();
    hasher.update(PACKAGE_DOMAIN);
    hash_bytes(&mut hasher, &manifest_bytes);
    encode_payload_tree(&mut hasher, payload_files);
    Ok(PackageDigest::from_hasher(hasher))
}

fn encode_payload_tree(hasher: &mut Sha256, payload_files: &[PayloadFile]) {
    hasher.update(PAYLOAD_DOMAIN);
    hash_u64(hasher, payload_files.len() as u64);
    for file in payload_files {
        hash_bytes(hasher, file.path.as_bytes());
        hasher.update([u8::from(file.executable)]);
        hash_bytes(hasher, &file.contents);
    }
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hash_u64(hasher, bytes.len() as u64);
    hasher.update(bytes);
}

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_be_bytes());
}

fn encode_string(output: &mut Vec<u8>, value: &str) {
    encode_u64(output, value.len() as u64);
    output.extend_from_slice(value.as_bytes());
}

fn encode_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_payload(root: &Path, reverse: bool) {
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("share/data")).unwrap();
        let files = if reverse {
            vec![
                ("share/data/value", b"payload\n".as_slice()),
                ("bin/live", b"runtime\n"),
            ]
        } else {
            vec![
                ("bin/live", b"runtime\n".as_slice()),
                ("share/data/value", b"payload\n"),
            ]
        };
        for (path, contents) in files {
            fs::write(root.join(path), contents).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.join("bin/live"), fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(
                root.join("share/data/value"),
                fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }
    }

    fn manifest_a(payload: &str) -> String {
        format!(
            r#"schema = "ocore.package/v1"
name = "personality/linux"
version = "0.1.0"
architecture = "x86_64"
payload_sha256 = "{payload}"

[runtime]
kind = "personality"
entry = "/bin/live"
abi = "ocore.personality/linux-x86_64-v1"

[[services]]
name = "z.service"
protocol = "ocore.z/v1"

[[services]]
name = "a.service"
protocol = "ocore.a/v1"

[[capability_requests]]
kind = "endpoint"
rights = ["send", "receive"]
purpose = "personality syscall request channel"

[health]
protocol = "ocore.health/v1"
timeout_ms = 2000

[build]
source_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
builder = "ocorec-host/v1"
"#
        )
    }

    fn manifest_b(payload: &str) -> String {
        format!(
            r#"name = "personality/linux"
schema = "ocore.package/v1"
payload_sha256 = "{payload}"
architecture = "x86_64"
version = "0.1.0"

[build]
builder = "ocorec-host/v1"
source_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[health]
timeout_ms = 2000
protocol = "ocore.health/v1"

[runtime]
abi = "ocore.personality/linux-x86_64-v1"
entry = "/bin/live"
kind = "personality"

[[capability_requests]]
purpose = "personality syscall request channel"
rights = ["receive", "send"]
kind = "endpoint"

[[services]]
protocol = "ocore.a/v1"
name = "a.service"

[[services]]
protocol = "ocore.z/v1"
name = "z.service"
"#
        )
    }

    fn parsed_manifest() -> PackageManifest {
        PackageManifest::parse_toml(&manifest_a(&"0".repeat(64))).unwrap()
    }

    fn assert_limit(error: PackageError, resource: &'static str) {
        assert!(matches!(
            error,
            PackageError::LimitExceeded {
                resource: actual,
                ..
            } if actual == resource
        ));
    }

    #[test]
    fn package_identity_is_deterministic_across_ordering() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        write_payload(first.path(), false);
        write_payload(second.path(), true);
        let first_payload = payload_sha256(first.path()).unwrap();
        let second_payload = payload_sha256(second.path()).unwrap();
        assert_eq!(first_payload, second_payload);

        let first_package =
            VerifiedPackage::load(&manifest_a(&first_payload), first.path()).unwrap();
        let second_package =
            VerifiedPackage::load(&manifest_b(&second_payload), second.path()).unwrap();
        assert_eq!(first_package.digest(), second_package.digest());
        assert_eq!(
            first_package.manifest().canonical_identity_bytes().unwrap(),
            second_package
                .manifest()
                .canonical_identity_bytes()
                .unwrap()
        );
    }

    #[test]
    fn payload_tampering_is_rejected() {
        let payload = TempDir::new().unwrap();
        write_payload(payload.path(), false);
        let digest = payload_sha256(payload.path()).unwrap();
        VerifiedPackage::load(&manifest_a(&digest), payload.path()).unwrap();

        fs::write(payload.path().join("share/data/value"), b"tampered\n").unwrap();
        assert!(matches!(
            VerifiedPackage::load(&manifest_a(&digest), payload.path()),
            Err(PackageError::PayloadDigestMismatch { .. })
        ));
    }

    #[test]
    fn unknown_manifest_field_is_rejected() {
        let payload = TempDir::new().unwrap();
        write_payload(payload.path(), false);
        let digest = payload_sha256(payload.path()).unwrap();
        let manifest = manifest_a(&digest).replacen(
            "schema = \"ocore.package/v1\"",
            "schema = \"ocore.package/v1\"\nunknown = true",
            1,
        );
        assert!(matches!(
            PackageManifest::parse_toml(&manifest),
            Err(PackageError::InvalidToml(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn payload_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let payload = TempDir::new().unwrap();
        fs::write(payload.path().join("real"), b"contents").unwrap();
        symlink(payload.path().join("real"), payload.path().join("link")).unwrap();
        assert!(matches!(
            payload_sha256(payload.path()),
            Err(PackageError::Symlink { .. })
        ));
    }

    #[test]
    fn traversal_and_duplicate_declarations_are_rejected() {
        assert!(matches!(
            normalize_relative_payload_path(Path::new("../escape")),
            Err(PackageError::InvalidPayloadPath { .. })
        ));

        let payload = TempDir::new().unwrap();
        write_payload(payload.path(), false);
        let digest = payload_sha256(payload.path()).unwrap();
        let duplicated =
            manifest_a(&digest).replace("name = \"a.service\"", "name = \"z.service\"");
        assert!(matches!(
            PackageManifest::parse_toml(&duplicated),
            Err(PackageError::Duplicate {
                field: "services.name",
                ..
            })
        ));
    }

    #[test]
    fn manifest_size_is_rejected_before_toml_parsing() {
        let oversized = "x".repeat(MAX_MANIFEST_BYTES + 1);
        assert_limit(
            PackageManifest::parse_toml(&oversized).unwrap_err(),
            "manifest bytes",
        );
    }

    #[test]
    fn declaration_count_limits_are_enforced() {
        let mut manifest = parsed_manifest();
        manifest.services = (0..=MAX_SERVICES)
            .map(|index| ServiceManifest {
                name: format!("service.{index}"),
                protocol: "ocore.service/v1".to_owned(),
            })
            .collect();
        assert_limit(manifest.validate().unwrap_err(), "service declarations");

        let mut manifest = parsed_manifest();
        manifest.capability_requests = (0..=MAX_CAPABILITY_REQUESTS)
            .map(|index| CapabilityRequestManifest {
                kind: "endpoint".to_owned(),
                rights: vec!["send".to_owned()],
                purpose: format!("request {index}"),
            })
            .collect();
        assert_limit(
            manifest.validate().unwrap_err(),
            "capability request declarations",
        );

        let mut manifest = parsed_manifest();
        manifest.capability_requests[0].rights = (0..=MAX_RIGHTS_PER_REQUEST)
            .map(|index| format!("right{index}"))
            .collect();
        assert_limit(
            manifest.validate().unwrap_err(),
            "rights per capability request",
        );
    }

    #[test]
    fn text_and_health_limits_are_enforced() {
        let mut manifest = parsed_manifest();
        manifest.name = "n".repeat(MAX_LOGICAL_NAME_BYTES + 1);
        assert_limit(manifest.validate().unwrap_err(), "name");

        let mut manifest = parsed_manifest();
        manifest.architecture = "a".repeat(MAX_IDENTIFIER_BYTES + 1);
        assert_limit(manifest.validate().unwrap_err(), "architecture");

        let mut manifest = parsed_manifest();
        manifest.services[0].protocol = "p".repeat(MAX_PROTOCOL_BYTES + 1);
        assert_limit(manifest.validate().unwrap_err(), "services.protocol");

        let mut manifest = parsed_manifest();
        manifest.capability_requests[0].purpose = "p".repeat(MAX_PURPOSE_BYTES + 1);
        assert_limit(
            manifest.validate().unwrap_err(),
            "capability_requests.purpose",
        );

        let mut manifest = parsed_manifest();
        manifest.runtime.entry = format!("/{}", "p".repeat(MAX_RUNTIME_ENTRY_BYTES));
        assert_limit(manifest.validate().unwrap_err(), "runtime.entry bytes");

        for invalid_timeout in [0, 60_001] {
            let mut manifest = parsed_manifest();
            manifest.health.timeout_ms = invalid_timeout;
            assert!(matches!(
                manifest.validate(),
                Err(PackageError::InvalidField {
                    field: "health.timeout_ms",
                    ..
                })
            ));
        }

        let manifest = parsed_manifest();
        assert!(manifest.canonical_toml().unwrap().len() <= MAX_MANIFEST_BYTES);
    }

    #[test]
    fn payload_path_rules_and_runtime_mapping_are_shared() {
        assert_eq!(
            runtime_entry_payload_path("/bin/live").unwrap(),
            PathBuf::from("bin/live")
        );
        for invalid in [
            "",
            "/absolute",
            "../escape",
            "./current",
            "a/../escape",
            "a/./current",
            "a//empty",
            "a\\portable",
            "a\0nul",
        ] {
            assert!(normalize_relative_payload_path(Path::new(invalid)).is_err());
        }
        for invalid in ["bin/live", "/", "/../escape", "/bin//live", "/bin\\live"] {
            assert!(runtime_entry_payload_path(invalid).is_err());
        }

        let too_deep = (0..=MAX_PAYLOAD_PATH_COMPONENTS)
            .map(|_| "d")
            .collect::<Vec<_>>()
            .join("/");
        assert!(normalize_relative_payload_path(Path::new(&too_deep)).is_err());
        assert!(runtime_entry_payload_path(&format!("/{too_deep}")).is_err());

        let long_component = "x".repeat(MAX_PAYLOAD_PATH_COMPONENT_BYTES + 1);
        assert!(normalize_relative_payload_path(Path::new(&long_component)).is_err());
        assert!(runtime_entry_payload_path(&format!("/{long_component}")).is_err());

        for invalid_name in [
            "../escape",
            "/absolute",
            "trailing/",
            "two//parts",
            "bad name",
        ] {
            assert!(validate_logical_name("name", invalid_name).is_err());
        }
    }

    #[test]
    fn payload_file_count_limit_is_preflighted() {
        let payload = TempDir::new().unwrap();
        for index in 0..=MAX_PAYLOAD_FILES {
            File::create(payload.path().join(format!("file-{index:04}"))).unwrap();
        }
        assert_limit(payload_sha256(payload.path()).unwrap_err(), "payload files");
    }

    #[test]
    fn payload_directory_entry_limit_blocks_empty_directory_bombs() {
        let payload = TempDir::new().unwrap();
        for index in 0..=MAX_PAYLOAD_ENTRIES {
            fs::create_dir(payload.path().join(format!("dir-{index:04}"))).unwrap();
        }
        assert_limit(
            payload_sha256(payload.path()).unwrap_err(),
            "payload directory entries",
        );
    }

    #[test]
    fn payload_file_and_total_byte_limits_are_preflighted() {
        let oversized_file = TempDir::new().unwrap();
        File::create(oversized_file.path().join("large"))
            .unwrap()
            .set_len(MAX_PAYLOAD_FILE_BYTES + 1)
            .unwrap();
        assert!(matches!(
            payload_sha256(oversized_file.path()),
            Err(PackageError::PayloadFileTooLarge { .. })
        ));

        let oversized_tree = TempDir::new().unwrap();
        for index in 0..5 {
            File::create(oversized_tree.path().join(format!("sparse-{index}")))
                .unwrap()
                .set_len(MAX_PAYLOAD_FILE_BYTES)
                .unwrap();
        }
        assert!(matches!(
            payload_sha256(oversized_tree.path()),
            Err(PackageError::PayloadTreeTooLarge { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_roots_non_utf8_names_and_special_files_are_rejected() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;

        let temporary = TempDir::new().unwrap();
        let real = temporary.path().join("real");
        fs::create_dir(&real).unwrap();
        let linked = temporary.path().join("linked");
        symlink(&real, &linked).unwrap();
        assert!(matches!(
            payload_sha256(&linked),
            Err(PackageError::Symlink { .. })
        ));

        let non_utf8 = PathBuf::from(OsString::from_vec(vec![0xff]));
        assert!(matches!(
            normalize_relative_payload_path(&non_utf8),
            Err(PackageError::NonUtf8Path { .. })
        ));

        let special = TempDir::new().unwrap();
        let _listener = UnixListener::bind(special.path().join("socket")).unwrap();
        assert!(matches!(
            payload_sha256(special.path()),
            Err(PackageError::UnsupportedFileType { .. })
        ));
    }
}
