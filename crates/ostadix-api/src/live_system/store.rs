//! Atomic local content-addressed storage for verified live-system packages.

use super::manifest::{
    normalize_relative_payload_path, validate_logical_name, PackageDigest, PackageError,
    PackageManifest, PayloadFile, VerifiedPackage, MAX_MANIFEST_BYTES, SHA256_ALGORITHM,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

pub const STORE_OBJECT_SCHEMA_V1: &str = "ocore.store-object/v1";
pub const STORE_ALIAS_SCHEMA_V1: &str = "ocore.store-alias/v1";
pub const OBJECT_METADATA_FILE: &str = "object.toml";
pub const OBJECT_MANIFEST_FILE: &str = "manifest.toml";
pub const OBJECT_PAYLOAD_DIRECTORY: &str = "payload";

const MAX_STORE_METADATA_BYTES: u64 = 4096;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A local store rooted at `objects/sha256` and `aliases`.
#[derive(Debug, Clone)]
pub struct PackageStore {
    root: PathBuf,
}

/// A package object that was verified at its current immutable store path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPackage {
    digest: PackageDigest,
    object_path: PathBuf,
    payload_path: PathBuf,
    manifest: PackageManifest,
}

impl StoredPackage {
    pub fn digest(&self) -> &PackageDigest {
        &self.digest
    }

    /// Root containing `object.toml`, `manifest.toml`, and `payload/`.
    pub fn path(&self) -> &Path {
        &self.object_path
    }

    /// Immutable root against which package-internal runtime entries resolve.
    pub fn payload_path(&self) -> &Path {
        &self.payload_path
    }

    pub fn manifest(&self) -> &PackageManifest {
        &self.manifest
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Package(#[from] PackageError),

    #[error("could not encode store metadata: {0}")]
    EncodeMetadata(#[from] toml::ser::Error),

    #[error("invalid store metadata at {path:?}: {source}")]
    DecodeMetadata {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("invalid store path {path:?}: {reason}")]
    UnsafePath { path: PathBuf, reason: String },

    #[error("invalid store metadata at {path:?}: {reason}")]
    InvalidMetadata { path: PathBuf, reason: String },

    #[error("store file {path:?} exceeds the {max}-byte limit (got {actual})")]
    MetadataTooLarge {
        path: PathBuf,
        max: u64,
        actual: u64,
    },

    #[error("package object {digest} does not exist")]
    ObjectNotFound { digest: PackageDigest },

    #[error("package object identity mismatch: expected {expected}, computed {actual}")]
    ObjectIdentityMismatch {
        expected: PackageDigest,
        actual: PackageDigest,
    },

    #[error("could not allocate a unique temporary path beneath {path:?}")]
    TemporaryPathExhausted { path: PathBuf },

    #[error("could not {operation} store path {path:?}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObjectRecord {
    schema: String,
    algorithm: String,
    digest: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AliasRecord {
    schema: String,
    alias: String,
    algorithm: String,
    digest: String,
}

impl PackageStore {
    /// Open or initialize a store. Existing managed paths must be real
    /// directories, never symbolic links.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let store = Self {
            root: root.as_ref().to_path_buf(),
        };
        store.ensure_layout()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Deterministic object root for a package identity.
    pub fn object_path(&self, digest: &PackageDigest) -> PathBuf {
        self.sha256_objects_path().join(digest.as_hex())
    }

    /// Test whether an object directory exists without treating existence as
    /// verification or authority.
    pub fn contains(&self, digest: &PackageDigest) -> Result<bool, StoreError> {
        self.ensure_layout()?;
        let path = self.object_path(digest);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                Err(StoreError::UnsafePath {
                    path,
                    reason: "object path must be a real directory".to_owned(),
                })
            }
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(StoreError::Io {
                operation: "inspect",
                path,
                source,
            }),
        }
    }

    /// Verify and atomically publish a manifest plus payload tree.
    pub fn install(
        &self,
        manifest_toml: &str,
        payload_root: &Path,
    ) -> Result<StoredPackage, StoreError> {
        let package = VerifiedPackage::load(manifest_toml, payload_root)?;
        self.publish(&package)
    }

    /// Atomically publish bytes already captured by [`VerifiedPackage::load`].
    pub fn publish(&self, package: &VerifiedPackage) -> Result<StoredPackage, StoreError> {
        self.ensure_layout()?;
        if self.contains(package.digest())? {
            return self.verify(package.digest());
        }

        let temporary = self.create_temporary_object_directory()?;
        if let Err(error) = self.write_temporary_object(&temporary, package) {
            cleanup_temporary_directory(&temporary);
            return Err(error);
        }

        let destination = self.object_path(package.digest());
        match fs::rename(&temporary, &destination) {
            Ok(()) => {
                sync_directory(&self.sha256_objects_path()).map_err(|source| StoreError::Io {
                    operation: "synchronize object directory",
                    path: self.sha256_objects_path(),
                    source,
                })?;
            }
            Err(source) => {
                cleanup_temporary_directory(&temporary);
                if self.contains(package.digest())? {
                    return self.verify(package.digest());
                }
                return Err(StoreError::Io {
                    operation: "publish object atomically",
                    path: destination,
                    source,
                });
            }
        }
        self.verify(package.digest())
    }

    /// Recompute the manifest, payload, and package identity at an object path.
    pub fn verify(&self, digest: &PackageDigest) -> Result<StoredPackage, StoreError> {
        self.ensure_layout()?;
        let object_path = self.object_path(digest);
        let object_metadata = match fs::symlink_metadata(&object_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::ObjectNotFound {
                    digest: digest.clone(),
                })
            }
            Err(source) => {
                return Err(StoreError::Io {
                    operation: "inspect object",
                    path: object_path,
                    source,
                })
            }
        };
        if object_metadata.file_type().is_symlink() || !object_metadata.is_dir() {
            return Err(StoreError::UnsafePath {
                path: object_path,
                reason: "object path must be a real directory".to_owned(),
            });
        }
        verify_object_shape(&object_path)?;

        let metadata_path = object_path.join(OBJECT_METADATA_FILE);
        let metadata_text = read_regular_text(&metadata_path, MAX_STORE_METADATA_BYTES)?;
        let record: ObjectRecord =
            toml::from_str(&metadata_text).map_err(|source| StoreError::DecodeMetadata {
                path: metadata_path.clone(),
                source,
            })?;
        validate_object_record(&record, digest, &metadata_path)?;

        let manifest_path = object_path.join(OBJECT_MANIFEST_FILE);
        let manifest_text = read_regular_text(&manifest_path, MAX_MANIFEST_BYTES as u64)?;
        let payload_path = object_path.join(OBJECT_PAYLOAD_DIRECTORY);
        let package = VerifiedPackage::load(&manifest_text, &payload_path)?;
        if package.digest() != digest {
            return Err(StoreError::ObjectIdentityMismatch {
                expected: digest.clone(),
                actual: package.digest().clone(),
            });
        }
        verify_read_only_tree(&object_path)?;

        Ok(StoredPackage {
            digest: digest.clone(),
            object_path,
            payload_path,
            manifest: package.manifest().clone(),
        })
    }

    /// Atomically point an unprivileged logical alias at an existing digest.
    /// Alias records contain identity metadata only and confer no capability.
    pub fn set_alias(&self, alias: &str, digest: &PackageDigest) -> Result<(), StoreError> {
        validate_logical_name("alias", alias)?;
        self.verify(digest)?;
        self.ensure_layout()?;

        let record = AliasRecord {
            schema: STORE_ALIAS_SCHEMA_V1.to_owned(),
            alias: alias.to_owned(),
            algorithm: SHA256_ALGORITHM.to_owned(),
            digest: digest.as_hex().to_owned(),
        };
        let encoded = toml::to_string(&record)?;
        let destination = self.alias_path(alias)?;
        if let Ok(metadata) = fs::symlink_metadata(&destination) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(StoreError::UnsafePath {
                    path: destination,
                    reason: "alias path must be a regular file".to_owned(),
                });
            }
        }

        let (temporary, mut file) = self.create_temporary_alias_file()?;
        let write_result = (|| -> Result<(), StoreError> {
            file.write_all(encoded.as_bytes())
                .map_err(|source| StoreError::Io {
                    operation: "write alias metadata",
                    path: temporary.clone(),
                    source,
                })?;
            file.sync_all().map_err(|source| StoreError::Io {
                operation: "synchronize alias metadata",
                path: temporary.clone(),
                source,
            })?;
            drop(file);
            fs::rename(&temporary, &destination).map_err(|source| StoreError::Io {
                operation: "replace alias atomically",
                path: destination.clone(),
                source,
            })?;
            sync_directory(&self.aliases_path()).map_err(|source| StoreError::Io {
                operation: "synchronize alias directory",
                path: self.aliases_path(),
                source,
            })?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result
    }

    /// Resolve an alias to a verified digest. The return value intentionally
    /// carries no authority or activation state.
    pub fn resolve_alias(&self, alias: &str) -> Result<Option<PackageDigest>, StoreError> {
        validate_logical_name("alias", alias)?;
        self.ensure_layout()?;
        let path = self.alias_path(alias)?;
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Io {
                    operation: "inspect alias",
                    path,
                    source,
                })
            }
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(StoreError::UnsafePath {
                    path,
                    reason: "alias path must be a regular file".to_owned(),
                })
            }
            Ok(_) => {}
        }

        let text = read_regular_text(&path, MAX_STORE_METADATA_BYTES)?;
        let record: AliasRecord =
            toml::from_str(&text).map_err(|source| StoreError::DecodeMetadata {
                path: path.clone(),
                source,
            })?;
        if record.schema != STORE_ALIAS_SCHEMA_V1
            || record.algorithm != SHA256_ALGORITHM
            || record.alias != alias
        {
            return Err(StoreError::InvalidMetadata {
                path,
                reason: "alias schema, algorithm, or logical name does not match".to_owned(),
            });
        }
        let digest = PackageDigest::from_hex(&record.digest)?;
        self.verify(&digest)?;
        Ok(Some(digest))
    }

    /// Hashed alias metadata path. Logical separators never become filesystem
    /// traversal because the validated alias itself is stored inside the file.
    pub fn alias_path(&self, alias: &str) -> Result<PathBuf, StoreError> {
        validate_logical_name("alias", alias)?;
        let hash = Sha256::digest(alias.as_bytes());
        Ok(self
            .aliases_path()
            .join(format!("{}.toml", hex::encode(hash))))
    }

    fn ensure_layout(&self) -> Result<(), StoreError> {
        ensure_real_directory(&self.root)?;
        ensure_real_directory(&self.root.join("objects"))?;
        ensure_real_directory(&self.sha256_objects_path())?;
        ensure_real_directory(&self.aliases_path())?;
        Ok(())
    }

    fn sha256_objects_path(&self) -> PathBuf {
        self.root.join("objects").join(SHA256_ALGORITHM)
    }

    fn aliases_path(&self) -> PathBuf {
        self.root.join("aliases")
    }

    fn create_temporary_object_directory(&self) -> Result<PathBuf, StoreError> {
        for _ in 0..128 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = self
                .sha256_objects_path()
                .join(format!(".tmp-{}-{sequence}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(StoreError::Io {
                        operation: "create temporary object directory",
                        path,
                        source,
                    })
                }
            }
        }
        Err(StoreError::TemporaryPathExhausted {
            path: self.sha256_objects_path(),
        })
    }

    fn create_temporary_alias_file(&self) -> Result<(PathBuf, File), StoreError> {
        for _ in 0..128 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = self
                .aliases_path()
                .join(format!(".tmp-alias-{}-{sequence}", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(StoreError::Io {
                        operation: "create temporary alias metadata",
                        path,
                        source,
                    })
                }
            }
        }
        Err(StoreError::TemporaryPathExhausted {
            path: self.aliases_path(),
        })
    }

    fn write_temporary_object(
        &self,
        temporary: &Path,
        package: &VerifiedPackage,
    ) -> Result<(), StoreError> {
        let payload_path = temporary.join(OBJECT_PAYLOAD_DIRECTORY);
        fs::create_dir(&payload_path).map_err(|source| StoreError::Io {
            operation: "create object payload directory",
            path: payload_path.clone(),
            source,
        })?;

        let record = ObjectRecord {
            schema: STORE_OBJECT_SCHEMA_V1.to_owned(),
            algorithm: SHA256_ALGORITHM.to_owned(),
            digest: package.digest().as_hex().to_owned(),
        };
        let record_toml = toml::to_string(&record)?;
        write_new_object_file(
            &temporary.join(OBJECT_METADATA_FILE),
            record_toml.as_bytes(),
            false,
        )?;
        let manifest_toml = package.manifest().canonical_toml()?;
        write_new_object_file(
            &temporary.join(OBJECT_MANIFEST_FILE),
            manifest_toml.as_bytes(),
            false,
        )?;

        for payload_file in package.payload_files() {
            write_payload_file(&payload_path, payload_file)?;
        }
        make_directory_tree_read_only(temporary)?;
        // File fsync alone is insufficient for crash-durable nested payloads:
        // every directory entry in the newly built tree must reach stable
        // storage before the object-root rename can become authoritative.
        sync_directory_tree_bottom_up(&payload_path)?;
        sync_directory(temporary).map_err(|source| StoreError::Io {
            operation: "synchronize temporary object",
            path: temporary.to_path_buf(),
            source,
        })?;
        Ok(())
    }
}

fn write_payload_file(payload_root: &Path, payload_file: &PayloadFile) -> Result<(), StoreError> {
    let relative = normalize_relative_payload_path(Path::new(payload_file.path()))?;
    let destination = payload_root.join(relative);
    let parent = destination
        .parent()
        .expect("validated payload paths always have a parent");
    fs::create_dir_all(parent).map_err(|source| StoreError::Io {
        operation: "create payload parent directory",
        path: parent.to_path_buf(),
        source,
    })?;
    write_new_object_file(
        &destination,
        payload_file.contents(),
        payload_file.is_executable(),
    )
}

fn write_new_object_file(path: &Path, contents: &[u8], executable: bool) -> Result<(), StoreError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| StoreError::Io {
            operation: "create object file",
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(contents).map_err(|source| StoreError::Io {
        operation: "write object file",
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| StoreError::Io {
        operation: "synchronize object file",
        path: path.to_path_buf(),
        source,
    })?;
    drop(file);
    set_object_file_read_only(path, executable)
}

fn validate_object_record(
    record: &ObjectRecord,
    digest: &PackageDigest,
    path: &Path,
) -> Result<(), StoreError> {
    if record.schema != STORE_OBJECT_SCHEMA_V1
        || record.algorithm != SHA256_ALGORITHM
        || record.digest != digest.as_hex()
    {
        return Err(StoreError::InvalidMetadata {
            path: path.to_path_buf(),
            reason: "object schema, algorithm, or digest does not match its path".to_owned(),
        });
    }
    Ok(())
}

fn verify_object_shape(object_path: &Path) -> Result<(), StoreError> {
    let mut names = BTreeSet::new();
    let entries = fs::read_dir(object_path).map_err(|source| StoreError::Io {
        operation: "read object directory",
        path: object_path.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| StoreError::Io {
            operation: "read object directory entry",
            path: object_path.to_path_buf(),
            source,
        })?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| StoreError::InvalidMetadata {
                path: entry.path(),
                reason: "object entry name is not valid UTF-8".to_owned(),
            })?;
        if !names.insert(name.clone()) {
            return Err(StoreError::InvalidMetadata {
                path: entry.path(),
                reason: format!("duplicate object entry `{name}`"),
            });
        }
        if !matches!(
            name.as_str(),
            OBJECT_METADATA_FILE | OBJECT_MANIFEST_FILE | OBJECT_PAYLOAD_DIRECTORY
        ) {
            return Err(StoreError::InvalidMetadata {
                path: entry.path(),
                reason: format!("unexpected object entry `{name}`"),
            });
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|source| StoreError::Io {
            operation: "inspect object entry",
            path: entry.path(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(StoreError::UnsafePath {
                path: entry.path(),
                reason: "symbolic links are forbidden in store objects".to_owned(),
            });
        }
        if name == OBJECT_PAYLOAD_DIRECTORY && !metadata.is_dir() {
            return Err(StoreError::InvalidMetadata {
                path: entry.path(),
                reason: "payload entry must be a directory".to_owned(),
            });
        }
        if name != OBJECT_PAYLOAD_DIRECTORY && !metadata.is_file() {
            return Err(StoreError::InvalidMetadata {
                path: entry.path(),
                reason: "object metadata entry must be a regular file".to_owned(),
            });
        }
    }
    let expected = BTreeSet::from([
        OBJECT_METADATA_FILE.to_owned(),
        OBJECT_MANIFEST_FILE.to_owned(),
        OBJECT_PAYLOAD_DIRECTORY.to_owned(),
    ]);
    if names != expected {
        return Err(StoreError::InvalidMetadata {
            path: object_path.to_path_buf(),
            reason: "object is missing metadata, manifest, or payload".to_owned(),
        });
    }
    Ok(())
}

fn read_regular_text(path: &Path, max_bytes: u64) -> Result<String, StoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| StoreError::Io {
        operation: "inspect store file",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            reason: "expected a regular file, not a symbolic link or special file".to_owned(),
        });
    }
    if metadata.len() > max_bytes {
        return Err(StoreError::MetadataTooLarge {
            path: path.to_path_buf(),
            max: max_bytes,
            actual: metadata.len(),
        });
    }
    let file = open_store_file(path).map_err(|source| StoreError::Io {
        operation: "open store file",
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| StoreError::Io {
            operation: "read store file",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > max_bytes {
        return Err(StoreError::MetadataTooLarge {
            path: path.to_path_buf(),
            max: max_bytes,
            actual: bytes.len() as u64,
        });
    }
    String::from_utf8(bytes).map_err(|error| StoreError::InvalidMetadata {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })
}

fn open_store_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn ensure_real_directory(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(StoreError::UnsafePath {
                path: path.to_path_buf(),
                reason: "managed store path must be a real directory".to_owned(),
            })
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(StoreError::Io {
                operation: "inspect managed directory",
                path: path.to_path_buf(),
                source,
            })
        }
    }
    fs::create_dir_all(path).map_err(|source| StoreError::Io {
        operation: "create managed directory",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|source| StoreError::Io {
        operation: "inspect created directory",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            reason: "created store path is not a real directory".to_owned(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn set_object_file_read_only(path: &Path, executable: bool) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o555 } else { 0o444 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|source| StoreError::Io {
        operation: "make object file read-only",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_object_file_read_only(path: &Path, _executable: bool) -> Result<(), StoreError> {
    let mut permissions = fs::metadata(path)
        .map_err(|source| StoreError::Io {
            operation: "inspect object file permissions",
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions).map_err(|source| StoreError::Io {
        operation: "make object file read-only",
        path: path.to_path_buf(),
        source,
    })
}

fn make_directory_tree_read_only(path: &Path) -> Result<(), StoreError> {
    let entries = fs::read_dir(path).map_err(|source| StoreError::Io {
        operation: "read object tree",
        path: path.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| StoreError::Io {
            operation: "read object tree entry",
            path: path.to_path_buf(),
            source,
        })?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|source| StoreError::Io {
            operation: "inspect object tree entry",
            path: entry.path(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(StoreError::UnsafePath {
                path: entry.path(),
                reason: "symbolic links are forbidden in store objects".to_owned(),
            });
        }
        if metadata.is_dir() {
            make_directory_tree_read_only(&entry.path())?;
        }
    }
    set_directory_read_only(path)
}

#[cfg(unix)]
fn set_directory_read_only(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o555)).map_err(|source| StoreError::Io {
        operation: "make object directory read-only",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_directory_read_only(path: &Path) -> Result<(), StoreError> {
    let mut permissions = fs::metadata(path)
        .map_err(|source| StoreError::Io {
            operation: "inspect object directory permissions",
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions).map_err(|source| StoreError::Io {
        operation: "make object directory read-only",
        path: path.to_path_buf(),
        source,
    })
}

fn verify_read_only_tree(path: &Path) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| StoreError::Io {
        operation: "inspect immutable object",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            reason: "symbolic links are forbidden in store objects".to_owned(),
        });
    }
    if is_writable(&metadata) {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            reason: "published object path is writable".to_owned(),
        });
    }
    if metadata.is_dir() {
        let entries = fs::read_dir(path).map_err(|source| StoreError::Io {
            operation: "read immutable object directory",
            path: path.to_path_buf(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| StoreError::Io {
                operation: "read immutable object directory entry",
                path: path.to_path_buf(),
                source,
            })?;
            verify_read_only_tree(&entry.path())?;
        }
    }
    Ok(())
}

fn sync_directory_tree_bottom_up(path: &Path) -> Result<(), StoreError> {
    let entries = fs::read_dir(path).map_err(|source| StoreError::Io {
        operation: "read directory for durable publication",
        path: path.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| StoreError::Io {
            operation: "read directory entry for durable publication",
            path: path.to_path_buf(),
            source,
        })?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path).map_err(|source| StoreError::Io {
            operation: "inspect directory entry for durable publication",
            path: entry_path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(StoreError::UnsafePath {
                path: entry_path,
                reason: "symbolic links are forbidden in store objects".to_owned(),
            });
        }
        if metadata.is_dir() {
            sync_directory_tree_bottom_up(&entry_path)?;
        }
    }
    sync_directory(path).map_err(|source| StoreError::Io {
        operation: "synchronize payload directory bottom-up",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn is_writable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o222 != 0
}

#[cfg(not(unix))]
fn is_writable(metadata: &fs::Metadata) -> bool {
    !metadata.permissions().readonly()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn cleanup_temporary_directory(path: &Path) {
    make_tree_writable_for_cleanup(path);
    let _ = fs::remove_dir_all(path);
}

fn make_tree_writable_for_cleanup(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.is_dir() {
        make_path_writable(path, true);
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                make_tree_writable_for_cleanup(&entry.path());
            }
        }
    } else if metadata.is_file() {
        make_path_writable(path, false);
    }
}

#[cfg(unix)]
fn make_path_writable(path: &Path, directory: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if directory { 0o700 } else { 0o600 };
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn make_path_writable(path: &Path, _directory: bool) {
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(false);
        let _ = fs::set_permissions(path, permissions);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_system::manifest::payload_sha256;
    use tempfile::TempDir;

    fn make_payload(root: &Path, contents: &[u8]) {
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin/live"), contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.join("bin/live"), fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn manifest(name: &str, payload: &str) -> String {
        format!(
            r#"schema = "ocore.package/v1"
name = "{name}"
version = "0.1.0"
architecture = "x86_64"
payload_sha256 = "{payload}"
services = []
capability_requests = []

[runtime]
kind = "native"
entry = "/bin/live"
abi = "ocore.native/v1"

[health]
protocol = "ocore.health/v1"
timeout_ms = 2000

[build]
source_sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
builder = "ocorec-host/v1"
"#
        )
    }

    #[test]
    fn publication_is_verified_atomic_and_read_only() {
        let temporary = TempDir::new().unwrap();
        let payload = temporary.path().join("source");
        make_payload(&payload, b"first\n");
        let payload_digest = payload_sha256(&payload).unwrap();
        let store = PackageStore::open(temporary.path().join("store")).unwrap();
        let stored = store
            .install(&manifest("runtime/first", &payload_digest), &payload)
            .unwrap();

        assert_eq!(
            stored.payload_path(),
            stored.path().join(OBJECT_PAYLOAD_DIRECTORY)
        );
        assert_eq!(store.verify(stored.digest()).unwrap(), stored);
        assert!(store.contains(stored.digest()).unwrap());
        assert!(fs::metadata(stored.path().join(OBJECT_MANIFEST_FILE))
            .unwrap()
            .permissions()
            .readonly());
        assert!(fs::read_dir(store.sha256_objects_path())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp-")));
    }

    #[test]
    fn nested_payload_directories_are_published_and_verified() {
        let temporary = TempDir::new().unwrap();
        let payload = temporary.path().join("source");
        make_payload(&payload, b"runtime\n");
        fs::create_dir_all(payload.join("share/world/config")).unwrap();
        fs::write(payload.join("share/world/config/runtime.toml"), b"stable\n").unwrap();
        let payload_digest = payload_sha256(&payload).unwrap();
        let store = PackageStore::open(temporary.path().join("store")).unwrap();

        let stored = store
            .install(&manifest("runtime/nested", &payload_digest), &payload)
            .unwrap();

        assert_eq!(
            fs::read(
                stored
                    .payload_path()
                    .join("share/world/config/runtime.toml")
            )
            .unwrap(),
            b"stable\n"
        );
        assert_eq!(store.verify(stored.digest()).unwrap(), stored);
    }

    #[test]
    fn moving_an_alias_does_not_change_object_identity() {
        let temporary = TempDir::new().unwrap();
        let first_payload = temporary.path().join("first");
        let second_payload = temporary.path().join("second");
        make_payload(&first_payload, b"first\n");
        make_payload(&second_payload, b"second\n");
        let first_hash = payload_sha256(&first_payload).unwrap();
        let second_hash = payload_sha256(&second_payload).unwrap();
        let store = PackageStore::open(temporary.path().join("store")).unwrap();
        let first = store
            .install(&manifest("runtime/first", &first_hash), &first_payload)
            .unwrap();
        let second = store
            .install(&manifest("runtime/second", &second_hash), &second_payload)
            .unwrap();
        let first_path = first.path().to_path_buf();
        let first_digest = first.digest().clone();

        store.set_alias("runtime/current", first.digest()).unwrap();
        assert_eq!(
            store.resolve_alias("runtime/current").unwrap(),
            Some(first_digest.clone())
        );
        store.set_alias("runtime/current", second.digest()).unwrap();
        assert_eq!(
            store.resolve_alias("runtime/current").unwrap(),
            Some(second.digest().clone())
        );
        assert_eq!(store.verify(&first_digest).unwrap().path(), first_path);
        assert_ne!(first.digest(), second.digest());
    }

    #[cfg(unix)]
    #[test]
    fn payload_tampering_and_writable_objects_are_denied() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = TempDir::new().unwrap();
        let payload = temporary.path().join("source");
        make_payload(&payload, b"original\n");
        let payload_digest = payload_sha256(&payload).unwrap();
        let store = PackageStore::open(temporary.path().join("store")).unwrap();
        let stored = store
            .install(&manifest("runtime/tamper", &payload_digest), &payload)
            .unwrap();
        let file = stored.payload_path().join("bin/live");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&file, b"tampered\n").unwrap();
        assert!(store.verify(stored.digest()).is_err());
    }

    #[test]
    fn alias_traversal_is_rejected() {
        let temporary = TempDir::new().unwrap();
        let store = PackageStore::open(temporary.path()).unwrap();
        assert!(store.alias_path("../escape").is_err());
    }
}
