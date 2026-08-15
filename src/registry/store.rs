use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::placement::NodeProfileV1;

use super::{
    canonical_registry_bytes, create_registry_root, merge_registry_store, verify_registry_store,
    ProfileStalenessPolicyV1, RegistryError, RegistryRootPinV1, RegistrySignerV1, RegistryStoreV1,
    RegistryTrustV1, VerifiedRegistryV1,
};

pub const MAX_REGISTRY_INPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROFILE_JSON_BYTES: usize = 2 * 1024 * 1024;
const KEY_MAGIC_V1: &[u8] = b"OSTADIX-REGISTRY-KEY-V1\0";
const SECRET_KEY_BYTES: usize = 32;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryStatePathsV1 {
    state: PathBuf,
    signing_key: PathBuf,
    trust: PathBuf,
}

impl RegistryStatePathsV1 {
    pub fn new(
        state: impl Into<PathBuf>,
        signing_key: impl Into<PathBuf>,
        trust: impl Into<PathBuf>,
    ) -> Self {
        Self {
            state: state.into(),
            signing_key: signing_key.into(),
            trust: trust.into(),
        }
    }

    pub fn state(&self) -> &Path {
        &self.state
    }

    pub fn signing_key(&self) -> &Path {
        &self.signing_key
    }

    pub fn trust(&self) -> &Path {
        &self.trust
    }
}

/// Initialize an independent registry root. Existing targets are never
/// overwritten; callers can safely retry after inspecting a partial I/O error.
pub fn write_new_registry_state(
    paths: &RegistryStatePathsV1,
    namespace: impl Into<String>,
    valid_from_ms: u64,
    expires_at_ms: u64,
    signer: &RegistrySignerV1,
) -> Result<RegistryStoreV1, RegistryError> {
    for path in [&paths.state, &paths.signing_key, &paths.trust] {
        if path.exists() {
            return Err(RegistryError::AlreadyExists(path.clone()));
        }
    }
    let snapshot = create_registry_root(namespace, valid_from_ms, expires_at_ms, signer)?;
    let root = snapshot
        .events()
        .first()
        .and_then(|event| match event.event().body() {
            super::RegistryEventBodyV1::NamespaceRoot(root) => Some(root),
            _ => None,
        })
        .ok_or(RegistryError::InvalidRootEvent)?;
    let trust = RegistryTrustV1::new([RegistryRootPinV1::new(
        root.namespace(),
        *root.public_key(),
    )?])?;
    let store = RegistryStoreV1::new(snapshot);

    write_new_file(&paths.signing_key, &encode_key(signer))?;
    if let Err(error) = write_new_canonical(&paths.state, &store) {
        let _ = fs::remove_file(&paths.signing_key);
        return Err(error);
    }
    if let Err(error) = write_new_canonical(&paths.trust, &trust) {
        let _ = fs::remove_file(&paths.state);
        let _ = fs::remove_file(&paths.signing_key);
        return Err(error);
    }
    Ok(store)
}

pub fn read_signing_key(path: impl AsRef<Path>) -> Result<RegistrySignerV1, RegistryError> {
    let path = path.as_ref();
    enforce_private_mode(path)?;
    let bytes = read_bounded(path, KEY_MAGIC_V1.len() + SECRET_KEY_BYTES)?;
    if bytes.len() != KEY_MAGIC_V1.len() + SECRET_KEY_BYTES || !bytes.starts_with(KEY_MAGIC_V1) {
        return Err(RegistryError::MalformedKey);
    }
    let mut secret = [0_u8; SECRET_KEY_BYTES];
    secret.copy_from_slice(&bytes[KEY_MAGIC_V1.len()..]);
    Ok(RegistrySignerV1::from_secret_bytes(secret))
}

pub fn read_registry_store(path: impl AsRef<Path>) -> Result<RegistryStoreV1, RegistryError> {
    read_canonical(path.as_ref(), MAX_REGISTRY_INPUT_BYTES)
}

pub fn read_registry_trust(path: impl AsRef<Path>) -> Result<RegistryTrustV1, RegistryError> {
    read_canonical(path.as_ref(), MAX_REGISTRY_INPUT_BYTES)
}

pub fn read_node_profile_json(path: impl AsRef<Path>) -> Result<NodeProfileV1, RegistryError> {
    let bytes = read_bounded(path.as_ref(), MAX_PROFILE_JSON_BYTES)?;
    serde_json::from_slice(&bytes).map_err(RegistryError::from)
}

pub fn atomic_write_node_profile_json(
    path: impl AsRef<Path>,
    profile: &NodeProfileV1,
) -> Result<(), RegistryError> {
    let mut bytes = serde_json::to_vec_pretty(profile)?;
    bytes.push(b'\n');
    atomic_write(path.as_ref(), &bytes)
}

pub fn atomic_write_registry_store(
    path: impl AsRef<Path>,
    store: &RegistryStoreV1,
) -> Result<(), RegistryError> {
    atomic_write(path.as_ref(), &canonical_registry_bytes(store)?)
}

pub fn atomic_write_registry_trust(
    path: impl AsRef<Path>,
    trust: &RegistryTrustV1,
) -> Result<(), RegistryError> {
    atomic_write(path.as_ref(), &canonical_registry_bytes(trust)?)
}

pub fn export_registry_store(
    store: &RegistryStoreV1,
    output: impl AsRef<Path>,
) -> Result<(), RegistryError> {
    store.validate_shape()?;
    atomic_write_registry_store(output, store)
}

pub fn import_registry_store(
    state_path: impl AsRef<Path>,
    incoming_path: impl AsRef<Path>,
    trust_path: impl AsRef<Path>,
    now_ms: u64,
    staleness: ProfileStalenessPolicyV1,
) -> Result<VerifiedRegistryV1, RegistryError> {
    let state_path = state_path.as_ref();
    let current = read_registry_store(state_path)?;
    let incoming = read_registry_store(incoming_path)?;
    let trust = read_registry_trust(trust_path)?;
    verify_registry_store(&current, &trust, now_ms, staleness)?;
    let merged = merge_registry_store(&current, &incoming)?;
    let verified = verify_registry_store(&merged, &trust, now_ms, staleness)?;
    atomic_write_registry_store(state_path, &merged)?;
    Ok(verified)
}

fn encode_key(signer: &RegistrySignerV1) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(KEY_MAGIC_V1.len() + SECRET_KEY_BYTES);
    bytes.extend_from_slice(KEY_MAGIC_V1);
    bytes.extend_from_slice(&signer.secret_bytes());
    bytes
}

fn read_canonical<T: DeserializeOwned + Serialize>(
    path: &Path,
    maximum: usize,
) -> Result<T, RegistryError> {
    let bytes = read_bounded(path, maximum)?;
    let value: T = crate::wire::decode_message(&bytes)
        .map_err(|error| RegistryError::Canonical(error.to_string()))?;
    if canonical_registry_bytes(&value)? != bytes {
        return Err(RegistryError::NonCanonicalEncoding);
    }
    Ok(value)
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, RegistryError> {
    let file = File::open(path).map_err(|error| RegistryError::io(path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| RegistryError::io(path, error))?;
    if metadata.len() > maximum as u64 {
        return Err(RegistryError::InputTooLarge {
            path: path.to_owned(),
            actual: metadata.len(),
            maximum,
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((maximum as u64) + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| RegistryError::io(path, error))?;
    if bytes.len() > maximum {
        return Err(RegistryError::InputTooLarge {
            path: path.to_owned(),
            actual: bytes.len() as u64,
            maximum,
        });
    }
    Ok(bytes)
}

fn write_new_canonical<T: Serialize>(path: &Path, value: &T) -> Result<(), RegistryError> {
    write_new_file(path, &canonical_registry_bytes(value)?)
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), RegistryError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            RegistryError::AlreadyExists(path.to_owned())
        } else {
            RegistryError::io(path, error)
        }
    })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| RegistryError::io(path, error))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), RegistryError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        RegistryError::Canonical(format!(
            "registry output path `{}` has no file name",
            path.display()
        ))
    })?;
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{}.tmp.{}.{}",
        file_name.to_string_lossy(),
        std::process::id(),
        counter
    ));
    write_new_file(&temporary, bytes)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(RegistryError::io(path, error));
    }
    sync_parent(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<(), RegistryError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| RegistryError::io(parent, error))
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<(), RegistryError> {
    Ok(())
}

#[cfg(unix)]
fn enforce_private_mode(path: &Path) -> Result<(), RegistryError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(path).map_err(|error| RegistryError::io(path, error))?;
    let mode = metadata.mode() & 0o777;
    if mode == 0o600 {
        Ok(())
    } else {
        Err(RegistryError::InsecureKeyPermissions {
            path: path.to_owned(),
            mode,
        })
    }
}

#[cfg(not(unix))]
fn enforce_private_mode(_path: &Path) -> Result<(), RegistryError> {
    Ok(())
}
