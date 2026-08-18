use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::id::domain_digest;
use super::{
    canonical_bytes, InformationErrorV1, RevisionIdV1, MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1,
    MAX_SIGNED_INFORMATION_PACK_BYTES_V1, MAX_T1_BYTES,
};

const INFORMATION_HEAD_BYTES_V1: usize = 65;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(
    Clone, Copy, Debug, serde::Deserialize, Eq, Ord, PartialEq, PartialOrd, serde::Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum InformationObjectKindV1 {
    Blob,
    Entity,
    Atom,
    Snapshot,
    Revision,
    ProjectionReceipt,
    Delta,
    Decision,
    Observation,
}

impl InformationObjectKindV1 {
    fn directory(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Entity => "entity",
            Self::Atom => "atom",
            Self::Snapshot => "snapshot",
            Self::Revision => "revision",
            Self::ProjectionReceipt => "projection",
            Self::Delta => "delta",
            Self::Decision => "decision",
            Self::Observation => "observation",
        }
    }

    pub(crate) fn domain(self) -> &'static [u8] {
        match self {
            Self::Blob => b"ostadix.info-blob/v1",
            Self::Entity => b"ostadix.info-entity/v1",
            Self::Atom => b"ostadix.info-atom/v1",
            Self::Snapshot => b"ostadix.info-snapshot/v1",
            Self::Revision => b"ostadix.info-revision/v1",
            Self::ProjectionReceipt => b"ostadix.info-projection/v1",
            Self::Delta => b"ostadix.info-delta/v1",
            Self::Decision => b"ostadix.info-decision/v1",
            Self::Observation => b"ostadix.info-observation/v1",
        }
    }
}

/// Experimental local content-addressed store for authority-free information.
///
/// Opening a root takes an exclusive process lock. The store contains no
/// execution capability and its named heads are local conveniences outside
/// canonical object identity.
pub struct InformationStoreV1 {
    root: PathBuf,
    lock: File,
}

/// Existing-root, lock-free reader for bounded information inspection.
///
/// Construction performs no directory creation, permission repair, lock-file
/// open, or head mutation. Writers remain responsible for synchronization;
/// every object read still verifies its content identity after the bounded
/// regular-file read.
#[derive(Clone, Debug)]
pub struct InformationStoreReaderV1 {
    root: PathBuf,
}

impl InformationStoreReaderV1 {
    pub fn open_existing(root: impl AsRef<Path>) -> Result<Self, InformationErrorV1> {
        let root = root.as_ref().to_path_buf();
        validate_existing_private_directory(&root)?;
        validate_existing_private_directory(&root.join("objects"))?;
        validate_existing_private_directory(&root.join("heads"))?;
        Ok(Self { root })
    }

    pub fn get(
        &self,
        kind: InformationObjectKindV1,
        sha256: &str,
    ) -> Result<Vec<u8>, InformationErrorV1> {
        validate_digest(sha256)?;
        let kind_directory = self.root.join("objects").join(kind.directory());
        validate_existing_private_directory(&kind_directory)?;
        let path = kind_directory.join(sha256);
        let bytes = read_regular_file(&path, maximum_object_bytes(kind))?;
        let actual = domain_digest(kind.domain(), &bytes);
        if actual != sha256 {
            return Err(InformationErrorV1::ObjectDigestMismatch {
                expected: sha256.to_string(),
                actual,
            });
        }
        Ok(bytes)
    }

    pub fn read_head(&self, name: &str) -> Result<Option<RevisionIdV1>, InformationErrorV1> {
        validate_head_name(name)?;
        let path = self.root.join("heads").join(name);
        let Some(bytes) = read_regular_file_if_exists(&path, INFORMATION_HEAD_BYTES_V1)? else {
            return Ok(None);
        };
        if bytes.len() != INFORMATION_HEAD_BYTES_V1 || bytes[64] != b'\n' {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "information head `{name}` must be exactly 64 lowercase hex bytes followed by one newline"
            )));
        }
        let text = std::str::from_utf8(&bytes[..64])
            .map_err(|error| InformationErrorV1::InvalidRecord(error.to_string()))?;
        RevisionIdV1::from_sha256(text.to_string()).map(Some)
    }
}

impl InformationStoreV1 {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, InformationErrorV1> {
        let root = root.as_ref().to_path_buf();
        ensure_private_directory(&root)?;
        ensure_private_directory(&root.join("objects"))?;
        ensure_private_directory(&root.join("heads"))?;

        let lock_path = root.join("store.lock");
        reject_symlink(&lock_path)?;
        let lock = open_private_file(&lock_path)?;
        lock.try_lock_exclusive().map_err(|error| {
            InformationErrorV1::StoreLocked(format!("{}: {error}", root.display()))
        })?;
        Ok(Self { root, lock })
    }

    pub fn put<T: Serialize>(
        &self,
        kind: InformationObjectKindV1,
        expected_sha256: &str,
        value: &T,
    ) -> Result<(), InformationErrorV1> {
        self.put_canonical(kind, expected_sha256, &canonical_bytes(value)?)
    }

    pub fn put_canonical(
        &self,
        kind: InformationObjectKindV1,
        expected_sha256: &str,
        bytes: &[u8],
    ) -> Result<(), InformationErrorV1> {
        validate_digest(expected_sha256)?;
        let maximum = maximum_object_bytes(kind);
        enforce_maximum("information object", bytes.len(), maximum)?;
        let actual = domain_digest(kind.domain(), bytes);
        if actual != expected_sha256 {
            return Err(InformationErrorV1::ObjectDigestMismatch {
                expected: expected_sha256.to_string(),
                actual,
            });
        }
        let directory = self.root.join("objects").join(kind.directory());
        ensure_private_directory(&directory)?;
        let destination = directory.join(expected_sha256);
        if let Some(existing) = read_regular_file_if_exists(&destination, maximum)? {
            if existing == bytes {
                return Ok(());
            }
            return Err(InformationErrorV1::ObjectDigestMismatch {
                expected: expected_sha256.to_string(),
                actual: domain_digest(kind.domain(), &existing),
            });
        }
        atomic_publish(&directory, &destination, bytes)
    }

    pub fn get(
        &self,
        kind: InformationObjectKindV1,
        sha256: &str,
    ) -> Result<Vec<u8>, InformationErrorV1> {
        validate_digest(sha256)?;
        let path = self
            .root
            .join("objects")
            .join(kind.directory())
            .join(sha256);
        let bytes = read_regular_file(&path, maximum_object_bytes(kind))?;
        let actual = domain_digest(kind.domain(), &bytes);
        if actual != sha256 {
            return Err(InformationErrorV1::ObjectDigestMismatch {
                expected: sha256.to_string(),
                actual,
            });
        }
        Ok(bytes)
    }

    pub fn read_head(&self, name: &str) -> Result<Option<RevisionIdV1>, InformationErrorV1> {
        validate_head_name(name)?;
        let path = self.root.join("heads").join(name);
        let Some(bytes) = read_regular_file_if_exists(&path, INFORMATION_HEAD_BYTES_V1)? else {
            return Ok(None);
        };
        if bytes.len() != INFORMATION_HEAD_BYTES_V1 || bytes[64] != b'\n' {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "information head `{name}` must be exactly 64 lowercase hex bytes followed by one newline"
            )));
        }
        let text = std::str::from_utf8(&bytes[..64])
            .map_err(|error| InformationErrorV1::InvalidRecord(error.to_string()))?;
        RevisionIdV1::from_sha256(text.to_string()).map(Some)
    }

    pub fn compare_and_set_head(
        &self,
        name: &str,
        expected: Option<&RevisionIdV1>,
        next: &RevisionIdV1,
    ) -> Result<(), InformationErrorV1> {
        validate_head_name(name)?;
        let current = self.read_head(name)?;
        if current.as_ref() != expected {
            return Err(InformationErrorV1::HeadConflict {
                name: name.to_string(),
                expected: expected.map(ToString::to_string),
                observed: current.map(|revision| revision.to_string()),
            });
        }
        self.get(InformationObjectKindV1::Revision, next.as_sha256())?;
        let directory = self.root.join("heads");
        let destination = directory.join(name);
        let bytes = format!("{}\n", next.as_sha256());
        atomic_publish(&directory, &destination, bytes.as_bytes())
    }

    /// Retain an already verified signed pack without promoting its objects or
    /// changing a local head. This is the conservative destination for stale
    /// or not-fully-typed offline information.
    pub fn archive_verified_pack(
        &self,
        expected_sha256: &str,
        bytes: &[u8],
    ) -> Result<(), InformationErrorV1> {
        validate_digest(expected_sha256)?;
        enforce_maximum(
            "signed historical information pack",
            bytes.len(),
            MAX_SIGNED_INFORMATION_PACK_BYTES_V1,
        )?;
        let actual = hex::encode(Sha256::digest(bytes));
        if actual != expected_sha256 {
            return Err(InformationErrorV1::ObjectDigestMismatch {
                expected: expected_sha256.to_string(),
                actual,
            });
        }
        let directory = self.root.join("historical-packs");
        ensure_private_directory(&directory)?;
        let destination = directory.join(format!("{expected_sha256}.cbor"));
        if let Some(existing) =
            read_regular_file_if_exists(&destination, MAX_SIGNED_INFORMATION_PACK_BYTES_V1)?
        {
            if existing == bytes {
                return Ok(());
            }
            return Err(InformationErrorV1::InvalidRecord(format!(
                "historical pack {} already exists with different bytes",
                destination.display()
            )));
        }
        atomic_publish(&directory, &destination, bytes)
    }
}

impl Drop for InformationStoreV1 {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock);
    }
}

fn validate_digest(value: &str) -> Result<(), InformationErrorV1> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(InformationErrorV1::InvalidDigest {
            kind: "object",
            value: value.to_string(),
        })
    }
}

fn validate_head_name(name: &str) -> Result<(), InformationErrorV1> {
    if !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(InformationErrorV1::InvalidRecord(format!(
            "invalid local information head name `{name}`"
        )))
    }
}

fn reject_symlink(path: &Path) -> Result<(), InformationErrorV1> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(InformationErrorV1::Io(format!(
            "refusing symlink {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(InformationErrorV1::Io(format!(
            "failed to inspect {}: {error}",
            path.display()
        ))),
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), InformationErrorV1> {
    reject_symlink(path)?;
    fs::create_dir_all(path).map_err(|error| {
        InformationErrorV1::Io(format!("failed to create {}: {error}", path.display()))
    })?;
    let metadata = fs::metadata(path).map_err(|error| {
        InformationErrorV1::Io(format!("failed to inspect {}: {error}", path.display()))
    })?;
    if !metadata.is_dir() {
        return Err(InformationErrorV1::Io(format!(
            "{} is not a directory",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            InformationErrorV1::Io(format!(
                "failed to set private permissions on {}: {error}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

fn validate_existing_private_directory(path: &Path) -> Result<(), InformationErrorV1> {
    reject_symlink(path)?;
    let metadata = fs::metadata(path).map_err(|error| {
        InformationErrorV1::Io(format!(
            "failed to inspect existing information directory {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(InformationErrorV1::Io(format!(
            "{} is not an existing directory",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(InformationErrorV1::Io(format!(
                "{} is not private to its owner",
                path.display()
            )));
        }
    }
    Ok(())
}

fn open_private_file(path: &Path) -> Result<File, InformationErrorV1> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(InformationErrorV1::Io(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(InformationErrorV1::Io(format!(
                "failed to inspect {}: {error}",
                path.display()
            )));
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|error| {
        InformationErrorV1::Io(format!("failed to open {}: {error}", path.display()))
    })?;
    let metadata = file.metadata().map_err(|error| {
        InformationErrorV1::Io(format!("failed to inspect {}: {error}", path.display()))
    })?;
    if !metadata.is_file() {
        return Err(InformationErrorV1::Io(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(InformationErrorV1::Io(format!(
                "{} is not private to its owner",
                path.display()
            )));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                InformationErrorV1::Io(format!(
                    "failed to set private permissions on {}: {error}",
                    path.display()
                ))
            })?;
    }
    Ok(file)
}

fn read_regular_file(path: &Path, maximum: usize) -> Result<Vec<u8>, InformationErrorV1> {
    read_regular_file_if_exists(path, maximum)?.ok_or_else(|| {
        InformationErrorV1::Io(format!("failed to open {}: not found", path.display()))
    })
}

fn read_regular_file_if_exists(
    path: &Path,
    maximum: usize,
) -> Result<Option<Vec<u8>>, InformationErrorV1> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(InformationErrorV1::Io(format!(
                "failed to open {}: {error}",
                path.display()
            )))
        }
    };
    let metadata = file.metadata().map_err(|error| {
        InformationErrorV1::Io(format!("failed to inspect {}: {error}", path.display()))
    })?;
    if !metadata.is_file() {
        return Err(InformationErrorV1::Io(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(InformationErrorV1::Io(format!(
                "{} is not private to its owner",
                path.display()
            )));
        }
    }
    let maximum_u64 = u64::try_from(maximum).map_err(|_| {
        InformationErrorV1::Io("information file-size limit does not fit u64".to_string())
    })?;
    if metadata.len() > maximum_u64 {
        return Err(InformationErrorV1::Io(format!(
            "{} has {} bytes; maximum is {maximum}",
            path.display(),
            metadata.len()
        )));
    }
    let read_limit = maximum_u64.checked_add(1).ok_or_else(|| {
        InformationErrorV1::Io("information file-size limit overflow".to_string())
    })?;
    let initial_capacity = usize::try_from(metadata.len())
        .unwrap_or(maximum)
        .min(maximum);
    let mut bytes = Vec::with_capacity(initial_capacity);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            InformationErrorV1::Io(format!("failed to read {}: {error}", path.display()))
        })?;
    if bytes.len() > maximum {
        return Err(InformationErrorV1::Io(format!(
            "{} exceeds the {maximum} byte limit while being read",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

fn enforce_maximum(label: &str, actual: usize, maximum: usize) -> Result<(), InformationErrorV1> {
    if actual <= maximum {
        Ok(())
    } else {
        Err(InformationErrorV1::Io(format!(
            "{label} has {actual} bytes; maximum is {maximum}"
        )))
    }
}

fn maximum_object_bytes(kind: InformationObjectKindV1) -> usize {
    if kind == InformationObjectKindV1::Blob {
        usize::try_from(MAX_T1_BYTES).unwrap_or(usize::MAX)
    } else {
        MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1
    }
}

fn atomic_publish(
    directory: &Path,
    destination: &Path,
    bytes: &[u8],
) -> Result<(), InformationErrorV1> {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(".tmp-{}-{sequence}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|error| {
        InformationErrorV1::Io(format!("failed to stage {}: {error}", temporary.display()))
    })?;
    let result = (|| {
        file.write_all(bytes).map_err(|error| {
            InformationErrorV1::Io(format!("failed to write {}: {error}", temporary.display()))
        })?;
        file.sync_all().map_err(|error| {
            InformationErrorV1::Io(format!("failed to sync {}: {error}", temporary.display()))
        })?;
        fs::rename(&temporary, destination).map_err(|error| {
            InformationErrorV1::Io(format!(
                "failed to publish {} as {}: {error}",
                temporary.display(),
                destination.display()
            ))
        })?;
        File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                InformationErrorV1::Io(format!(
                    "failed to sync directory {}: {error}",
                    directory.display()
                ))
            })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::information::{AtomIdV1, BlobIdV1, InformationRevisionV1, InformationSnapshotV1};

    #[test]
    fn object_storage_is_content_addressed_and_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let store = InformationStoreV1::open(root.path()).unwrap();
        let bytes = b"canonical object";
        let id = AtomIdV1::digest(bytes);
        store
            .put_canonical(InformationObjectKindV1::Atom, id.as_sha256(), bytes)
            .unwrap();
        store
            .put_canonical(InformationObjectKindV1::Atom, id.as_sha256(), bytes)
            .unwrap();
        assert_eq!(
            store
                .get(InformationObjectKindV1::Atom, id.as_sha256())
                .unwrap(),
            bytes
        );
        assert!(store
            .put_canonical(InformationObjectKindV1::Atom, &"00".repeat(32), bytes)
            .is_err());
    }

    #[test]
    fn object_and_historical_pack_reads_and_writes_share_hard_caps() {
        let root = tempfile::tempdir().unwrap();
        let store = InformationStoreV1::open(root.path()).unwrap();
        let oversized_object = vec![0_u8; MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1 + 1];
        let object_id = AtomIdV1::digest(&oversized_object);
        assert!(store
            .put_canonical(
                InformationObjectKindV1::Atom,
                object_id.as_sha256(),
                &oversized_object,
            )
            .is_err());

        let object_directory = root.path().join("objects/atom");
        fs::create_dir_all(&object_directory).unwrap();
        let object_path = object_directory.join(object_id.as_sha256());
        fs::write(&object_path, &oversized_object).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&object_path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(store
            .get(InformationObjectKindV1::Atom, object_id.as_sha256())
            .is_err());

        let oversized_pack = vec![0_u8; MAX_SIGNED_INFORMATION_PACK_BYTES_V1 + 1];
        let pack_sha256 = hex::encode(Sha256::digest(&oversized_pack));
        assert!(store
            .archive_verified_pack(&pack_sha256, &oversized_pack)
            .is_err());
    }

    #[test]
    fn managed_blob_store_preserves_the_exact_t1_capacity_boundary() {
        let root = tempfile::tempdir().unwrap();
        let store = InformationStoreV1::open(root.path()).unwrap();
        let boundary = vec![0x5a; usize::try_from(MAX_T1_BYTES).unwrap()];
        let blob_id = BlobIdV1::from_content_bytes(&boundary);
        store
            .put_canonical(
                InformationObjectKindV1::Blob,
                blob_id.as_sha256(),
                &boundary,
            )
            .unwrap();
        assert_eq!(
            store
                .get(InformationObjectKindV1::Blob, blob_id.as_sha256())
                .unwrap(),
            boundary
        );

        let oversized = vec![0_u8; usize::try_from(MAX_T1_BYTES).unwrap() + 1];
        let oversized_id = BlobIdV1::from_content_bytes(&oversized);
        assert!(store
            .put_canonical(
                InformationObjectKindV1::Blob,
                oversized_id.as_sha256(),
                &oversized,
            )
            .is_err());
    }

    #[test]
    fn local_heads_use_exact_compare_and_set() {
        let root = tempfile::tempdir().unwrap();
        let store = InformationStoreV1::open(root.path()).unwrap();
        let snapshot = InformationSnapshotV1::new(vec![]);
        let snapshot_id = snapshot.id().unwrap();
        store
            .put(
                InformationObjectKindV1::Snapshot,
                snapshot_id.as_sha256(),
                &snapshot,
            )
            .unwrap();
        let first_revision = InformationRevisionV1::new(snapshot_id.clone(), vec![], None).unwrap();
        let first = first_revision.id().unwrap();
        store
            .put(
                InformationObjectKindV1::Revision,
                first.as_sha256(),
                &first_revision,
            )
            .unwrap();
        let second_revision =
            InformationRevisionV1::new(snapshot_id, vec![first.clone()], None).unwrap();
        let second = second_revision.id().unwrap();
        store
            .put(
                InformationObjectKindV1::Revision,
                second.as_sha256(),
                &second_revision,
            )
            .unwrap();
        store.compare_and_set_head("main", None, &first).unwrap();
        let nonexistent = RevisionIdV1::from_sha256("33".repeat(32)).unwrap();
        assert!(store
            .compare_and_set_head("main", Some(&first), &nonexistent)
            .is_err());
        assert_eq!(store.read_head("main").unwrap(), Some(first.clone()));
        assert!(store.compare_and_set_head("main", None, &second).is_err());
        store
            .compare_and_set_head("main", Some(&first), &second)
            .unwrap();
        assert_eq!(store.read_head("main").unwrap(), Some(second));
    }

    #[test]
    fn local_heads_require_exact_canonical_file_bytes() {
        let root = tempfile::tempdir().unwrap();
        let store = InformationStoreV1::open(root.path()).unwrap();
        let path = root.path().join("heads/main");
        fs::write(&path, format!("{}\n\n", "11".repeat(32))).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(store.read_head("main").is_err());
        fs::write(&path, "11".repeat(32)).unwrap();
        assert!(store.read_head("main").is_err());
        fs::write(&path, format!("{}\n", "AA".repeat(32))).unwrap();
        assert!(store.read_head("main").is_err());
    }

    #[test]
    fn a_second_store_cannot_share_the_same_root() {
        let root = tempfile::tempdir().unwrap();
        let _first = InformationStoreV1::open(root.path()).unwrap();
        assert!(matches!(
            InformationStoreV1::open(root.path()),
            Err(InformationErrorV1::StoreLocked(_))
        ));
    }

    #[test]
    fn lock_path_must_be_a_regular_file() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("store.lock")).unwrap();
        assert!(matches!(
            InformationStoreV1::open(root.path()),
            Err(InformationErrorV1::Io(message)) if message.contains("not a regular file")
        ));
    }
}
