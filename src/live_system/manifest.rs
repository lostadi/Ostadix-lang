//! Strict `ocore.package/v1` manifests and deterministic package identities.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// The only package-manifest schema accepted by this foundation.
pub const PACKAGE_SCHEMA_V1: &str = "ocore.package/v1";
/// The digest algorithm used for payload and package identities.
pub const SHA256_ALGORITHM: &str = "sha256";

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
    InvalidField {
        field: &'static str,
        reason: String,
    },

    #[error("invalid SHA-256 in `{field}`: expected 64 lowercase hexadecimal characters")]
    InvalidSha256 { field: &'static str },

    #[error("duplicate value `{value}` in manifest field `{field}`")]
    Duplicate {
        field: &'static str,
        value: String,
    },

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
        validate_text("runtime.abi", &self.runtime.abi)?;

        let mut service_names = BTreeSet::new();
        for service in &self.services {
            validate_identifier("services.name", &service.name)?;
            validate_text("services.protocol", &service.protocol)?;
            if !service_names.insert(service.name.as_str()) {
                return Err(PackageError::Duplicate {
                    field: "services.name",
                    value: service.name.clone(),
                });
            }
        }

        let mut requests = BTreeSet::new();
        for request in &self.capability_requests {
            validate_identifier("capability_requests.kind", &request.kind)?;
            validate_text("capability_requests.purpose", &request.purpose)?;
            if request.rights.is_empty() {
                return Err(PackageError::InvalidField {
                    field: "capability_requests.rights",
                    reason: "must contain at least one right".to_owned(),
                });
            }

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

        validate_text("health.protocol", &self.health.protocol)?;
        if self.health.timeout_ms == 0 {
            return Err(PackageError::InvalidField {
                field: "health.timeout_ms",
                reason: "must be greater than zero".to_owned(),
            });
        }

        validate_sha256("build.source_sha256", &self.build.source_sha256)?;
        validate_text("build.builder", &self.build.builder)?;
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
        Ok(toml::to_string(&self.canonicalized())?)
    }

    fn canonicalized(&self) -> Self {
        let mut manifest = self.clone();
        manifest
            .services
            .sort_by(|left, right| (&left.name, &left.protocol).cmp(&(&right.name, &right.protocol)));
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

pub(crate) fn validate_logical_name(
    field: &'static str,
    value: &str,
) -> Result<(), PackageError> {
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
            character.is_ascii_alphanumeric()
                || matches!(character, '.' | '_' | '-' | '+' | '@')
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
    for component in raw.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(PackageError::InvalidPayloadPath {
                path: path.to_path_buf(),
                reason: "contains an empty, current-directory, or parent-directory component"
                    .to_owned(),
            });
        }
    }
    Ok(raw.to_owned())
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), PackageError> {
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

fn validate_text(field: &'static str, value: &str) -> Result<(), PackageError> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(PackageError::InvalidField {
            field,
            reason: "must be non-empty and contain no control characters".to_owned(),
        });
    }
    Ok(())
}

fn validate_runtime_entry(value: &str) -> Result<(), PackageError> {
    if !value.starts_with('/') || value == "/" || value.contains('\\') || value.contains('\0') {
        return Err(PackageError::InvalidField {
            field: "runtime.entry",
            reason: "must be an absolute payload path".to_owned(),
        });
    }
    if value[1..]
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(PackageError::InvalidField {
            field: "runtime.entry",
            reason: "contains an empty, current-directory, or parent-directory component"
                .to_owned(),
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
    let root_metadata = fs::symlink_metadata(payload_root).map_err(|source| PackageError::Io {
        operation: "inspect",
        path: payload_root.to_path_buf(),
        source,
    })?;
    if root_metadata.file_type().is_symlink() {
        return Err(PackageError::Symlink {
            path: payload_root.to_path_buf(),
        });
    }
    if !root_metadata.is_dir() {
        return Err(PackageError::PayloadRootNotDirectory {
            path: payload_root.to_path_buf(),
        });
    }

    let mut payload_files = Vec::new();
    let mut seen = BTreeSet::new();
    scan_directory(payload_root, payload_root, &mut payload_files, &mut seen)?;
    payload_files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(payload_files)
}

fn scan_directory(
    payload_root: &Path,
    directory: &Path,
    payload_files: &mut Vec<PayloadFile>,
    seen: &mut BTreeSet<String>,
) -> Result<(), PackageError> {
    let entries = fs::read_dir(directory).map_err(|source| PackageError::Io {
        operation: "read directory",
        path: directory.to_path_buf(),
        source,
    })?;
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| PackageError::Io {
            operation: "read directory entry",
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let relative = path
            .strip_prefix(payload_root)
            .expect("directory walk stays beneath payload root");
        let normalized = normalize_relative_payload_path(relative)?;
        children.push((normalized, path));
    }
    children.sort_by(|left, right| left.0.cmp(&right.0));

    for (normalized, path) in children {
        let metadata = fs::symlink_metadata(&path).map_err(|source| PackageError::Io {
            operation: "inspect",
            path: path.clone(),
            source,
        })?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            return Err(PackageError::Symlink { path });
        }
        if file_type.is_dir() {
            scan_directory(payload_root, &path, payload_files, seen)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(PackageError::UnsupportedFileType { path });
        }
        if !seen.insert(normalized.clone()) {
            return Err(PackageError::DuplicatePayloadPath { path: normalized });
        }

        let mut file = open_payload_file(&path).map_err(|source| PackageError::Io {
            operation: "open",
            path: path.clone(),
            source,
        })?;
        let opened_metadata = file.metadata().map_err(|source| PackageError::Io {
            operation: "inspect opened file",
            path: path.clone(),
            source,
        })?;
        if !opened_metadata.is_file() {
            return Err(PackageError::UnsupportedFileType { path });
        }
        let executable = is_executable(&opened_metadata);
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .map_err(|source| PackageError::Io {
                operation: "read",
                path: path.clone(),
                source,
            })?;
        let closed_metadata = file.metadata().map_err(|source| PackageError::Io {
            operation: "reinspect opened file",
            path: path.clone(),
            source,
        })?;
        if opened_metadata.len() != closed_metadata.len()
            || closed_metadata.len() != contents.len() as u64
        {
            return Err(PackageError::PayloadChanged { path });
        }
        payload_files.push(PayloadFile {
            path: normalized,
            executable,
            contents,
        });
    }
    Ok(())
}

fn open_payload_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
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
            vec![("share/data/value", b"payload\n".as_slice()), ("bin/live", b"runtime\n")]
        } else {
            vec![("bin/live", b"runtime\n".as_slice()), ("share/data/value", b"payload\n")]
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

    #[test]
    fn package_identity_is_deterministic_across_ordering() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        write_payload(first.path(), false);
        write_payload(second.path(), true);
        let first_payload = payload_sha256(first.path()).unwrap();
        let second_payload = payload_sha256(second.path()).unwrap();
        assert_eq!(first_payload, second_payload);

        let first_package = VerifiedPackage::load(&manifest_a(&first_payload), first.path()).unwrap();
        let second_package =
            VerifiedPackage::load(&manifest_b(&second_payload), second.path()).unwrap();
        assert_eq!(first_package.digest(), second_package.digest());
        assert_eq!(
            first_package.manifest().canonical_identity_bytes().unwrap(),
            second_package.manifest().canonical_identity_bytes().unwrap()
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
        let duplicated = manifest_a(&digest).replace(
            "name = \"a.service\"",
            "name = \"z.service\"",
        );
        assert!(matches!(
            PackageManifest::parse_toml(&duplicated),
            Err(PackageError::Duplicate {
                field: "services.name",
                ..
            })
        ));
    }
}
