//! Typed, authority-free boot objects backed by a strict read-only blob CAS.
//!
//! The canonical index binds one exact Git commit/tree to every tracked blob
//! path while storing duplicate blob contents only once by raw SHA-256.  Paths
//! are descriptive bindings, executable bits are inert metadata, and neither
//! an [`ObjectIdentity`] nor a [`PortableOValue::ObjectRef`] grants execution or
//! any other authority.

use crate::value::{OBytes, OText};
use crate::world::{
    ExtensionEnvelope, ObjectId, ObjectIdentity, ObjectVersion, PortableOValue, PortableValueError,
    PortableValueRecord, WorldId, WorldIdentityError,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const BOOT_OBJECT_INDEX_MAGIC: &[u8; 8] = b"OBOIDX\0\0";
pub const BOOT_OBJECT_INDEX_VERSION: u16 = 1;
pub const BOOT_OBJECT_INDEX_HEADER_BYTES: u16 = 80;
pub const BOOT_OBJECT_INDEX_DIGEST_BYTES: usize = 32;
pub const BOOT_OBJECT_INDEX_DIGEST_DOMAIN: &[u8] = b"ostadix.boot-object-index/v1\0";
pub const BOOT_OBJECT_INDEX_FILE: &str = "index.bin";
pub const BOOT_OBJECT_DIRECTORY: &str = "objects";
pub const BOOT_OBJECT_SHA256_DIRECTORY: &str = "sha256";
pub const BOOT_OBJECT_STORE_ENV: &str = "OSTADIX_BOOT_OBJECT_STORE";
pub const DEFAULT_BOOT_OBJECT_STORE: &str = "/usr/share/ostadix/boot-objects/v1";

pub const BOOT_OBJECT_REF_NAMESPACE_V1: &str = "org.ostadix.boot";
pub const BOOT_OBJECT_REF_NAME_V1: &str = "git-blob-ref";
pub const BOOT_OBJECT_REF_VERSION_V1: u16 = 1;
pub const BOOT_OBJECT_WORLD_V1: &str = "ostadix-boot-cas";
pub const BOOT_OBJECT_REF_SCHEMA_V1: &[u8] = b"ostadix.boot-object-ref/v1\0identity:object-ref\0kind:git-blob\0sha256:bytes32\0bytes:u64\0set-sha256:bytes32\0";

pub const MAX_BOOT_OBJECT_INDEX_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_BOOT_OBJECTS: usize = 4096;
pub const MAX_BOOT_BINDINGS: usize = 4096;
pub const MAX_BOOT_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_BOOT_OBJECT_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_BOOT_OBJECT_PATH_BYTES: usize = 4096;
pub const MAX_BOOT_OBJECT_PATH_COMPONENTS: usize = 32;
pub const MAX_BOOT_OBJECT_PATH_COMPONENT_BYTES: usize = 255;

const OBJECT_RECORD_BYTES: usize = 60;
const BINDING_PREFIX_BYTES: usize = 38;
const MIN_INDEX_BYTES: usize =
    BOOT_OBJECT_INDEX_HEADER_BYTES as usize + BOOT_OBJECT_INDEX_DIGEST_BYTES;

#[derive(Debug, Error)]
pub enum BootObjectError {
    #[error("invalid boot-object index: {0}")]
    InvalidIndex(String),

    #[error("boot-object {resource} exceeds its limit of {limit} (got {actual})")]
    LimitExceeded {
        resource: &'static str,
        limit: u64,
        actual: u64,
    },

    #[error("invalid boot-object path `{path}`: {reason}")]
    InvalidPath { path: String, reason: String },

    #[error("boot object sha256:{digest} is absent from the index")]
    UnknownObject { digest: String },

    #[error("boot-object path `{0}` is absent from the index")]
    UnknownPath(String),

    #[error("Git blob sha1:{0} is absent from the index")]
    UnknownGitObject(String),

    #[error("unsafe boot-object store path {path:?}: {reason}")]
    UnsafePath { path: PathBuf, reason: String },

    #[error("boot object {algorithm} mismatch for {path:?}: expected {expected}, got {actual}")]
    ObjectDigestMismatch {
        path: PathBuf,
        algorithm: &'static str,
        expected: String,
        actual: String,
    },

    #[error("boot object length mismatch for {path:?}: expected {expected}, got {actual}")]
    ObjectLengthMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },

    #[error("could not {operation} boot-object path {path:?}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error(transparent)]
    Identity(#[from] WorldIdentityError),

    #[error(transparent)]
    PortableValue(#[from] PortableValueError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum BootFileMode {
    Regular = 0o100644,
    Executable = 0o100755,
}

impl BootFileMode {
    pub const fn as_git_mode(self) -> u32 {
        self as u32
    }

    pub const fn is_executable(self) -> bool {
        matches!(self, Self::Executable)
    }

    pub const fn as_octal(self) -> &'static str {
        match self {
            Self::Regular => "100644",
            Self::Executable => "100755",
        }
    }
}

impl TryFrom<u32> for BootFileMode {
    type Error = BootObjectError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0o100644 => Ok(Self::Regular),
            0o100755 => Ok(Self::Executable),
            _ => Err(BootObjectError::InvalidIndex(format!(
                "unsupported Git file mode {value:#o}"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootObjectRecord {
    sha256: [u8; 32],
    git_sha1: [u8; 20],
    bytes: u64,
}

impl BootObjectRecord {
    pub fn new(sha256: [u8; 32], git_sha1: [u8; 20], bytes: u64) -> Result<Self, BootObjectError> {
        if sha256.iter().all(|byte| *byte == 0) {
            return Err(invalid_index("object SHA-256 cannot be all zero"));
        }
        if git_sha1.iter().all(|byte| *byte == 0) {
            return Err(invalid_index("Git blob SHA-1 cannot be all zero"));
        }
        enforce_limit("bytes", bytes, MAX_BOOT_OBJECT_BYTES)?;
        Ok(Self {
            sha256,
            git_sha1,
            bytes,
        })
    }

    pub const fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    pub const fn git_sha1(&self) -> &[u8; 20] {
        &self.git_sha1
    }

    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn sha256_hex(&self) -> String {
        hex::encode(self.sha256)
    }

    pub fn git_sha1_hex(&self) -> String {
        hex::encode(self.git_sha1)
    }

    pub fn identity(&self) -> Result<ObjectIdentity, BootObjectError> {
        boot_object_identity(&self.sha256)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootPathBinding {
    path: String,
    mode: BootFileMode,
    object_sha256: [u8; 32],
}

impl BootPathBinding {
    pub fn new(
        path: impl Into<String>,
        mode: BootFileMode,
        object_sha256: [u8; 32],
    ) -> Result<Self, BootObjectError> {
        let path = path.into();
        validate_boot_path(&path)?;
        if object_sha256.iter().all(|byte| *byte == 0) {
            return Err(invalid_index("binding object SHA-256 cannot be all zero"));
        }
        Ok(Self {
            path,
            mode,
            object_sha256,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub const fn mode(&self) -> BootFileMode {
        self.mode
    }

    pub const fn object_sha256(&self) -> &[u8; 32] {
        &self.object_sha256
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootObjectIndex {
    source_commit: [u8; 20],
    source_tree: [u8; 20],
    objects: Vec<BootObjectRecord>,
    bindings: Vec<BootPathBinding>,
    logical_bytes: u64,
    stored_bytes: u64,
    root_sha256: [u8; 32],
}

impl BootObjectIndex {
    pub fn new(
        source_commit: [u8; 20],
        source_tree: [u8; 20],
        mut objects: Vec<BootObjectRecord>,
        mut bindings: Vec<BootPathBinding>,
    ) -> Result<Self, BootObjectError> {
        validate_source_identity("source commit", &source_commit)?;
        validate_source_identity("source tree", &source_tree)?;
        enforce_limit(
            "object count",
            objects.len() as u64,
            MAX_BOOT_OBJECTS as u64,
        )?;
        enforce_limit(
            "binding count",
            bindings.len() as u64,
            MAX_BOOT_BINDINGS as u64,
        )?;

        objects.sort_by_key(|object| object.sha256);
        bindings.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
        validate_objects_and_bindings(&objects, &bindings)?;
        let stored_bytes = checked_sum_objects(&objects)?;
        let logical_bytes = checked_sum_bindings(&objects, &bindings)?;

        let mut index = Self {
            source_commit,
            source_tree,
            objects,
            bindings,
            logical_bytes,
            stored_bytes,
            root_sha256: [0; 32],
        };
        let prefix = index.canonical_prefix()?;
        index.root_sha256 = index_digest(&prefix);
        Ok(index)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, BootObjectError> {
        enforce_limit(
            "index bytes",
            bytes.len() as u64,
            MAX_BOOT_OBJECT_INDEX_BYTES as u64,
        )?;
        if bytes.len() < MIN_INDEX_BYTES {
            return Err(invalid_index(format!(
                "record is {} bytes; minimum is {MIN_INDEX_BYTES}",
                bytes.len()
            )));
        }
        let digest_offset = bytes.len() - BOOT_OBJECT_INDEX_DIGEST_BYTES;
        let (prefix, encoded_digest) = bytes.split_at(digest_offset);
        let actual_digest = index_digest(prefix);
        if encoded_digest != actual_digest {
            return Err(invalid_index(format!(
                "root SHA-256 mismatch: expected {}, got {}",
                hex::encode(encoded_digest),
                hex::encode(actual_digest)
            )));
        }

        let mut cursor = IndexCursor::new(prefix);
        if cursor.take(BOOT_OBJECT_INDEX_MAGIC.len())? != BOOT_OBJECT_INDEX_MAGIC {
            return Err(invalid_index("wrong index magic"));
        }
        let version = cursor.take_u16()?;
        if version != BOOT_OBJECT_INDEX_VERSION {
            return Err(invalid_index(format!(
                "unsupported index version {version}"
            )));
        }
        let header_bytes = cursor.take_u16()?;
        if header_bytes != BOOT_OBJECT_INDEX_HEADER_BYTES {
            return Err(invalid_index(format!(
                "header length is {header_bytes}; expected {BOOT_OBJECT_INDEX_HEADER_BYTES}"
            )));
        }
        let total_bytes = cursor.take_u32()? as usize;
        if total_bytes != bytes.len() {
            return Err(invalid_index(format!(
                "declared length is {total_bytes}; actual length is {}",
                bytes.len()
            )));
        }
        let source_commit = cursor.take_array::<20>()?;
        let source_tree = cursor.take_array::<20>()?;
        let object_count = cursor.take_u32()? as usize;
        let binding_count = cursor.take_u32()? as usize;
        let declared_logical_bytes = cursor.take_u64()?;
        let declared_stored_bytes = cursor.take_u64()?;
        if cursor.offset() != BOOT_OBJECT_INDEX_HEADER_BYTES as usize {
            return Err(invalid_index("internal header-length disagreement"));
        }
        enforce_limit("object count", object_count as u64, MAX_BOOT_OBJECTS as u64)?;
        enforce_limit(
            "binding count",
            binding_count as u64,
            MAX_BOOT_BINDINGS as u64,
        )?;
        let minimum_tables = object_count
            .checked_mul(OBJECT_RECORD_BYTES)
            .and_then(|value| {
                binding_count
                    .checked_mul(BINDING_PREFIX_BYTES)
                    .and_then(|bindings| value.checked_add(bindings))
            })
            .ok_or_else(|| invalid_index("table lengths overflow usize"))?;
        if cursor.remaining() < minimum_tables {
            return Err(invalid_index("declared tables exceed the record length"));
        }

        let mut objects = Vec::with_capacity(object_count);
        let mut prior_object = None;
        for _ in 0..object_count {
            let sha256 = cursor.take_array::<32>()?;
            if prior_object
                .as_ref()
                .is_some_and(|prior: &[u8; 32]| prior >= &sha256)
            {
                return Err(invalid_index(
                    "object SHA-256 records are not in strict ascending order",
                ));
            }
            prior_object = Some(sha256);
            objects.push(BootObjectRecord::new(
                sha256,
                cursor.take_array::<20>()?,
                cursor.take_u64()?,
            )?);
        }

        let mut bindings = Vec::with_capacity(binding_count);
        let mut prior_path: Option<Vec<u8>> = None;
        for _ in 0..binding_count {
            let path_bytes = cursor.take_u16()? as usize;
            let mode = BootFileMode::try_from(cursor.take_u32()?)?;
            let object_sha256 = cursor.take_array::<32>()?;
            let raw_path = cursor.take(path_bytes)?;
            if prior_path
                .as_ref()
                .is_some_and(|prior| prior.as_slice() >= raw_path)
            {
                return Err(invalid_index(
                    "path bindings are not in strict ascending UTF-8 byte order",
                ));
            }
            let path = std::str::from_utf8(raw_path)
                .map_err(|_| invalid_index("path binding is not valid UTF-8"))?
                .to_owned();
            prior_path = Some(raw_path.to_vec());
            bindings.push(BootPathBinding::new(path, mode, object_sha256)?);
        }
        if cursor.remaining() != 0 {
            return Err(invalid_index(format!(
                "record contains {} trailing bytes before its digest",
                cursor.remaining()
            )));
        }

        let index = Self::new(source_commit, source_tree, objects, bindings)?;
        if index.logical_bytes != declared_logical_bytes {
            return Err(invalid_index(format!(
                "logical byte total is {declared_logical_bytes}; computed {}",
                index.logical_bytes
            )));
        }
        if index.stored_bytes != declared_stored_bytes {
            return Err(invalid_index(format!(
                "stored byte total is {declared_stored_bytes}; computed {}",
                index.stored_bytes
            )));
        }
        if index.root_sha256 != actual_digest {
            return Err(invalid_index(
                "canonical re-encoding changed the root digest",
            ));
        }
        Ok(index)
    }

    pub fn encode(&self) -> Result<Vec<u8>, BootObjectError> {
        validate_objects_and_bindings(&self.objects, &self.bindings)?;
        if checked_sum_objects(&self.objects)? != self.stored_bytes
            || checked_sum_bindings(&self.objects, &self.bindings)? != self.logical_bytes
        {
            return Err(invalid_index("cached byte totals are not canonical"));
        }
        let mut output = self.canonical_prefix()?;
        let digest = index_digest(&output);
        if digest != self.root_sha256 {
            return Err(invalid_index("cached root SHA-256 is not canonical"));
        }
        output.extend_from_slice(&digest);
        Ok(output)
    }

    pub const fn source_commit(&self) -> &[u8; 20] {
        &self.source_commit
    }

    pub const fn source_tree(&self) -> &[u8; 20] {
        &self.source_tree
    }

    pub fn objects(&self) -> &[BootObjectRecord] {
        &self.objects
    }

    pub fn bindings(&self) -> &[BootPathBinding] {
        &self.bindings
    }

    pub const fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }

    pub const fn stored_bytes(&self) -> u64 {
        self.stored_bytes
    }

    pub const fn root_sha256(&self) -> &[u8; 32] {
        &self.root_sha256
    }

    pub fn object_by_sha256(&self, digest: &[u8; 32]) -> Option<&BootObjectRecord> {
        self.objects
            .binary_search_by(|object| object.sha256.cmp(digest))
            .ok()
            .map(|index| &self.objects[index])
    }

    pub fn object_by_git_sha1(&self, oid: &[u8; 20]) -> Option<&BootObjectRecord> {
        self.objects.iter().find(|object| &object.git_sha1 == oid)
    }

    pub fn binding_by_path(&self, path: &str) -> Option<&BootPathBinding> {
        self.bindings
            .binary_search_by(|binding| binding.path.as_bytes().cmp(path.as_bytes()))
            .ok()
            .map(|index| &self.bindings[index])
    }

    pub fn bindings_for_object<'a>(
        &'a self,
        digest: &'a [u8; 32],
    ) -> impl Iterator<Item = &'a BootPathBinding> + 'a {
        self.bindings
            .iter()
            .filter(move |binding| &binding.object_sha256 == digest)
    }

    fn canonical_prefix(&self) -> Result<Vec<u8>, BootObjectError> {
        let table_bytes = self
            .objects
            .len()
            .checked_mul(OBJECT_RECORD_BYTES)
            .and_then(|value| {
                self.bindings.iter().try_fold(value, |total, binding| {
                    total.checked_add(BINDING_PREFIX_BYTES + binding.path.len())
                })
            })
            .ok_or_else(|| invalid_index("encoded index length overflows usize"))?;
        let total_bytes = (BOOT_OBJECT_INDEX_HEADER_BYTES as usize)
            .checked_add(table_bytes)
            .and_then(|value| value.checked_add(BOOT_OBJECT_INDEX_DIGEST_BYTES))
            .ok_or_else(|| invalid_index("encoded index length overflows usize"))?;
        enforce_limit(
            "index bytes",
            total_bytes as u64,
            MAX_BOOT_OBJECT_INDEX_BYTES as u64,
        )?;
        let total_u32 = u32::try_from(total_bytes)
            .map_err(|_| invalid_index("encoded index length exceeds u32"))?;
        let object_count = u32::try_from(self.objects.len())
            .map_err(|_| invalid_index("object count exceeds u32"))?;
        let binding_count = u32::try_from(self.bindings.len())
            .map_err(|_| invalid_index("binding count exceeds u32"))?;

        let mut output = Vec::with_capacity(total_bytes - BOOT_OBJECT_INDEX_DIGEST_BYTES);
        output.extend_from_slice(BOOT_OBJECT_INDEX_MAGIC);
        put_u16(&mut output, BOOT_OBJECT_INDEX_VERSION);
        put_u16(&mut output, BOOT_OBJECT_INDEX_HEADER_BYTES);
        put_u32(&mut output, total_u32);
        output.extend_from_slice(&self.source_commit);
        output.extend_from_slice(&self.source_tree);
        put_u32(&mut output, object_count);
        put_u32(&mut output, binding_count);
        put_u64(&mut output, self.logical_bytes);
        put_u64(&mut output, self.stored_bytes);
        debug_assert_eq!(output.len(), BOOT_OBJECT_INDEX_HEADER_BYTES as usize);

        for object in &self.objects {
            output.extend_from_slice(&object.sha256);
            output.extend_from_slice(&object.git_sha1);
            put_u64(&mut output, object.bytes);
        }
        for binding in &self.bindings {
            let path_bytes = u16::try_from(binding.path.len())
                .map_err(|_| invalid_index("path length exceeds u16"))?;
            put_u16(&mut output, path_bytes);
            put_u32(&mut output, binding.mode.as_git_mode());
            output.extend_from_slice(&binding.object_sha256);
            output.extend_from_slice(binding.path.as_bytes());
        }
        Ok(output)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootObjectVerification {
    pub root_sha256: [u8; 32],
    pub object_count: usize,
    pub binding_count: usize,
    pub logical_bytes: u64,
    pub stored_bytes: u64,
}

#[derive(Debug)]
pub struct BootObjectStore {
    root: PathBuf,
    index: BootObjectIndex,
}

impl BootObjectStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, BootObjectError> {
        let root = root.as_ref().to_path_buf();
        require_real_directory(&root)?;
        require_exact_names(
            &root,
            BTreeSet::from([
                BOOT_OBJECT_INDEX_FILE.to_owned(),
                BOOT_OBJECT_DIRECTORY.to_owned(),
            ]),
        )?;
        let objects = root.join(BOOT_OBJECT_DIRECTORY);
        require_real_directory(&objects)?;
        require_exact_names(
            &objects,
            BTreeSet::from([BOOT_OBJECT_SHA256_DIRECTORY.to_owned()]),
        )?;
        require_real_directory(&objects.join(BOOT_OBJECT_SHA256_DIRECTORY))?;
        let index_path = root.join(BOOT_OBJECT_INDEX_FILE);
        let index_bytes = read_regular_bounded(&index_path, MAX_BOOT_OBJECT_INDEX_BYTES as u64)?;
        let index = BootObjectIndex::parse(&index_bytes)?;
        Ok(Self { root, index })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn index(&self) -> &BootObjectIndex {
        &self.index
    }

    pub fn object_path(&self, digest: &[u8; 32]) -> PathBuf {
        self.root
            .join(BOOT_OBJECT_DIRECTORY)
            .join(BOOT_OBJECT_SHA256_DIRECTORY)
            .join(hex::encode(digest))
    }

    pub fn read_object(&self, digest: &[u8; 32]) -> Result<Vec<u8>, BootObjectError> {
        let record =
            self.index
                .object_by_sha256(digest)
                .ok_or_else(|| BootObjectError::UnknownObject {
                    digest: hex::encode(digest),
                })?;
        self.read_verified_record(record)
    }

    pub fn read_path(&self, path: &str) -> Result<Vec<u8>, BootObjectError> {
        let binding = self
            .index
            .binding_by_path(path)
            .ok_or_else(|| BootObjectError::UnknownPath(path.to_owned()))?;
        self.read_object(binding.object_sha256())
    }

    pub fn verify(&self) -> Result<BootObjectVerification, BootObjectError> {
        let object_root = self
            .root
            .join(BOOT_OBJECT_DIRECTORY)
            .join(BOOT_OBJECT_SHA256_DIRECTORY);
        let expected = self
            .index
            .objects
            .iter()
            .map(BootObjectRecord::sha256_hex)
            .collect::<BTreeSet<_>>();
        let actual = regular_file_names(&object_root)?;
        if actual != expected {
            let missing = expected
                .difference(&actual)
                .take(8)
                .cloned()
                .collect::<Vec<_>>();
            let extra = actual
                .difference(&expected)
                .take(8)
                .cloned()
                .collect::<Vec<_>>();
            return Err(invalid_index(format!(
                "store object closure differs from index (missing={missing:?}, extra={extra:?})"
            )));
        }
        for object in &self.index.objects {
            self.read_verified_record(object)?;
        }
        Ok(BootObjectVerification {
            root_sha256: self.index.root_sha256,
            object_count: self.index.objects.len(),
            binding_count: self.index.bindings.len(),
            logical_bytes: self.index.logical_bytes,
            stored_bytes: self.index.stored_bytes,
        })
    }

    fn read_verified_record(&self, record: &BootObjectRecord) -> Result<Vec<u8>, BootObjectError> {
        let path = self.object_path(&record.sha256);
        let bytes = read_regular_bounded(&path, MAX_BOOT_OBJECT_BYTES)?;
        if bytes.len() as u64 != record.bytes {
            return Err(BootObjectError::ObjectLengthMismatch {
                path,
                expected: record.bytes,
                actual: bytes.len() as u64,
            });
        }
        let actual_sha256: [u8; 32] = Sha256::digest(&bytes).into();
        if actual_sha256 != record.sha256 {
            return Err(BootObjectError::ObjectDigestMismatch {
                path,
                algorithm: "SHA-256",
                expected: hex::encode(record.sha256),
                actual: hex::encode(actual_sha256),
            });
        }
        let actual_git_sha1 = git_blob_sha1(&bytes);
        if actual_git_sha1 != record.git_sha1 {
            return Err(BootObjectError::ObjectDigestMismatch {
                path,
                algorithm: "Git blob SHA-1",
                expected: hex::encode(record.git_sha1),
                actual: hex::encode(actual_git_sha1),
            });
        }
        Ok(bytes)
    }
}

pub fn boot_object_identity(digest: &[u8; 32]) -> Result<ObjectIdentity, BootObjectError> {
    Ok(ObjectIdentity::new(
        WorldId::new(BOOT_OBJECT_WORLD_V1)?,
        ObjectId::new(format!("git-blob-sha256-{}", hex::encode(digest)))?,
        ObjectVersion::new(1)?,
    ))
}

pub fn boot_object_ref_schema_sha256() -> [u8; 32] {
    Sha256::digest(BOOT_OBJECT_REF_SCHEMA_V1).into()
}

pub fn portable_boot_object_ref(
    set_sha256: [u8; 32],
    object: &BootObjectRecord,
) -> Result<PortableValueRecord, BootObjectError> {
    let value = PortableOValue::record(vec![
        ("bytes".to_owned(), PortableOValue::integer(object.bytes)?),
        (
            "identity".to_owned(),
            PortableOValue::ObjectRef(object.identity()?),
        ),
        (
            "kind".to_owned(),
            PortableOValue::text(OText {
                utf8: "git-blob".to_owned(),
                encoding: None,
            })?,
        ),
        (
            "set-sha256".to_owned(),
            PortableOValue::bytes(OBytes {
                bytes: set_sha256.to_vec(),
                media_type: None,
            })?,
        ),
        (
            "sha256".to_owned(),
            PortableOValue::bytes(OBytes {
                bytes: object.sha256.to_vec(),
                media_type: None,
            })?,
        ),
    ])?;
    Ok(PortableValueRecord::Extension(ExtensionEnvelope::new(
        BOOT_OBJECT_REF_NAMESPACE_V1,
        BOOT_OBJECT_REF_NAME_V1,
        BOOT_OBJECT_REF_VERSION_V1,
        boot_object_ref_schema_sha256(),
        value,
    )?))
}

fn validate_objects_and_bindings(
    objects: &[BootObjectRecord],
    bindings: &[BootPathBinding],
) -> Result<(), BootObjectError> {
    enforce_limit(
        "object count",
        objects.len() as u64,
        MAX_BOOT_OBJECTS as u64,
    )?;
    enforce_limit(
        "binding count",
        bindings.len() as u64,
        MAX_BOOT_BINDINGS as u64,
    )?;
    let mut prior_object = None;
    let mut git_oids = BTreeSet::new();
    let mut object_digests = BTreeSet::new();
    for object in objects {
        BootObjectRecord::new(object.sha256, object.git_sha1, object.bytes)?;
        if prior_object
            .as_ref()
            .is_some_and(|prior: &[u8; 32]| prior >= &object.sha256)
        {
            return Err(invalid_index(
                "object records are not in strict ascending SHA-256 order",
            ));
        }
        prior_object = Some(object.sha256);
        if !git_oids.insert(object.git_sha1) {
            return Err(invalid_index("duplicate Git blob SHA-1"));
        }
        object_digests.insert(object.sha256);
    }

    let mut prior_path: Option<&[u8]> = None;
    let mut referenced = BTreeSet::new();
    for binding in bindings {
        validate_boot_path(&binding.path)?;
        if prior_path.is_some_and(|prior| prior >= binding.path.as_bytes()) {
            return Err(invalid_index(
                "bindings are not in strict ascending path order",
            ));
        }
        prior_path = Some(binding.path.as_bytes());
        if !object_digests.contains(&binding.object_sha256) {
            return Err(invalid_index(format!(
                "path `{}` refers to absent object sha256:{}",
                binding.path,
                hex::encode(binding.object_sha256)
            )));
        }
        referenced.insert(binding.object_sha256);
    }
    if referenced != object_digests {
        return Err(invalid_index(
            "object table contains one or more unreferenced objects",
        ));
    }
    Ok(())
}

fn checked_sum_objects(objects: &[BootObjectRecord]) -> Result<u64, BootObjectError> {
    let total = objects.iter().try_fold(0_u64, |total, object| {
        total
            .checked_add(object.bytes)
            .ok_or_else(|| invalid_index("stored byte total overflows u64"))
    })?;
    enforce_limit("stored bytes", total, MAX_BOOT_OBJECT_TOTAL_BYTES)?;
    Ok(total)
}

fn checked_sum_bindings(
    objects: &[BootObjectRecord],
    bindings: &[BootPathBinding],
) -> Result<u64, BootObjectError> {
    let lengths = objects
        .iter()
        .map(|object| (object.sha256, object.bytes))
        .collect::<BTreeMap<_, _>>();
    let total = bindings.iter().try_fold(0_u64, |total, binding| {
        let bytes = lengths
            .get(&binding.object_sha256)
            .ok_or_else(|| invalid_index("binding refers to an absent object"))?;
        total
            .checked_add(*bytes)
            .ok_or_else(|| invalid_index("logical byte total overflows u64"))
    })?;
    enforce_limit("logical bytes", total, MAX_BOOT_OBJECT_TOTAL_BYTES)?;
    Ok(total)
}

fn validate_source_identity(
    label: &'static str,
    identity: &[u8; 20],
) -> Result<(), BootObjectError> {
    if identity.iter().all(|byte| *byte == 0) {
        return Err(invalid_index(format!("{label} cannot be all zero")));
    }
    Ok(())
}

fn validate_boot_path(path: &str) -> Result<(), BootObjectError> {
    let invalid = |reason: &str| BootObjectError::InvalidPath {
        path: path.to_owned(),
        reason: reason.to_owned(),
    };
    if path.is_empty() || path.starts_with('/') {
        return Err(invalid("must be a non-empty relative path"));
    }
    if path.contains('\\') || path.contains('\0') {
        return Err(invalid("backslashes and NUL bytes are forbidden"));
    }
    enforce_limit(
        "path bytes",
        path.len() as u64,
        MAX_BOOT_OBJECT_PATH_BYTES as u64,
    )?;
    let components = path.split('/').collect::<Vec<_>>();
    enforce_limit(
        "path components",
        components.len() as u64,
        MAX_BOOT_OBJECT_PATH_COMPONENTS as u64,
    )?;
    for component in components {
        if component.is_empty() || matches!(component, "." | "..") {
            return Err(invalid(
                "contains an empty, current-directory, or parent-directory component",
            ));
        }
        if component.len() > MAX_BOOT_OBJECT_PATH_COMPONENT_BYTES {
            return Err(invalid("contains an overlong path component"));
        }
    }
    Ok(())
}

fn enforce_limit(resource: &'static str, actual: u64, limit: u64) -> Result<(), BootObjectError> {
    if actual > limit {
        return Err(BootObjectError::LimitExceeded {
            resource,
            limit,
            actual,
        });
    }
    Ok(())
}

fn invalid_index(message: impl Into<String>) -> BootObjectError {
    BootObjectError::InvalidIndex(message.into())
}

fn index_digest(prefix: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(BOOT_OBJECT_INDEX_DIGEST_DOMAIN);
    hasher.update(prefix);
    hasher.finalize().into()
}

fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

struct IndexCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> IndexCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], BootObjectError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or_else(|| invalid_index("cursor length overflow"))?;
        if end > self.bytes.len() {
            return Err(invalid_index(format!(
                "truncated record at byte {}: need {count}, have {}",
                self.offset,
                self.bytes.len().saturating_sub(self.offset)
            )));
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], BootObjectError> {
        self.take(N)?
            .try_into()
            .map_err(|_| invalid_index("fixed-width field length mismatch"))
    }

    fn take_u16(&mut self) -> Result<u16, BootObjectError> {
        Ok(u16::from_be_bytes(self.take_array()?))
    }

    fn take_u32(&mut self) -> Result<u32, BootObjectError> {
        Ok(u32::from_be_bytes(self.take_array()?))
    }

    fn take_u64(&mut self) -> Result<u64, BootObjectError> {
        Ok(u64::from_be_bytes(self.take_array()?))
    }

    const fn offset(&self) -> usize {
        self.offset
    }

    const fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }
}

fn require_real_directory(path: &Path) -> Result<(), BootObjectError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| BootObjectError::Io {
        operation: "inspect",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(BootObjectError::UnsafePath {
            path: path.to_path_buf(),
            reason: "expected a real directory, not a symlink or special file".to_owned(),
        });
    }
    Ok(())
}

fn require_exact_names(path: &Path, expected: BTreeSet<String>) -> Result<(), BootObjectError> {
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(path).map_err(|source| BootObjectError::Io {
        operation: "read directory",
        path: path.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| BootObjectError::Io {
            operation: "read directory entry",
            path: path.to_path_buf(),
            source,
        })?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| BootObjectError::UnsafePath {
                path: entry.path(),
                reason: "directory entry name is not valid UTF-8".to_owned(),
            })?;
        actual.insert(name);
    }
    if actual != expected {
        return Err(BootObjectError::UnsafePath {
            path: path.to_path_buf(),
            reason: format!("expected exact entries {expected:?}, got {actual:?}"),
        });
    }
    Ok(())
}

fn regular_file_names(path: &Path) -> Result<BTreeSet<String>, BootObjectError> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(path).map_err(|source| BootObjectError::Io {
        operation: "read object directory",
        path: path.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| BootObjectError::Io {
            operation: "read object directory entry",
            path: path.to_path_buf(),
            source,
        })?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path).map_err(|source| BootObjectError::Io {
            operation: "inspect object entry",
            path: entry_path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(BootObjectError::UnsafePath {
                path: entry_path,
                reason: "object entry must be a regular non-symlink file".to_owned(),
            });
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| BootObjectError::UnsafePath {
                path: entry.path(),
                reason: "object name is not valid UTF-8".to_owned(),
            })?;
        if name.len() != 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(BootObjectError::UnsafePath {
                path: entry.path(),
                reason: "object name must be 64 lowercase hexadecimal characters".to_owned(),
            });
        }
        names.insert(name);
    }
    Ok(names)
}

fn read_regular_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, BootObjectError> {
    let path_metadata = fs::symlink_metadata(path).map_err(|source| BootObjectError::Io {
        operation: "inspect",
        path: path.to_path_buf(),
        source,
    })?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(BootObjectError::UnsafePath {
            path: path.to_path_buf(),
            reason: "expected a regular non-symlink file".to_owned(),
        });
    }
    enforce_limit("file bytes", path_metadata.len(), max_bytes)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path).map_err(|source| BootObjectError::Io {
        operation: "open",
        path: path.to_path_buf(),
        source,
    })?;
    read_open_file_bounded(file, path, &path_metadata, max_bytes)
}

fn read_open_file_bounded(
    mut file: File,
    path: &Path,
    path_metadata: &fs::Metadata,
    max_bytes: u64,
) -> Result<Vec<u8>, BootObjectError> {
    let before = file.metadata().map_err(|source| BootObjectError::Io {
        operation: "inspect opened descriptor",
        path: path.to_path_buf(),
        source,
    })?;
    if !before.is_file() {
        return Err(BootObjectError::UnsafePath {
            path: path.to_path_buf(),
            reason: "opened descriptor is not a regular file".to_owned(),
        });
    }
    require_same_opened_file(path, path_metadata, &before)?;
    enforce_limit("file bytes", before.len(), max_bytes)?;
    let expected_bytes = before.len();
    let mut bytes = Vec::with_capacity(expected_bytes as usize);
    file.by_ref()
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| BootObjectError::Io {
            operation: "read",
            path: path.to_path_buf(),
            source,
        })?;
    enforce_limit("file bytes", bytes.len() as u64, max_bytes)?;
    if bytes.len() as u64 != expected_bytes {
        return Err(BootObjectError::ObjectLengthMismatch {
            path: path.to_path_buf(),
            expected: expected_bytes,
            actual: bytes.len() as u64,
        });
    }
    let after = file.metadata().map_err(|source| BootObjectError::Io {
        operation: "reinspect opened descriptor",
        path: path.to_path_buf(),
        source,
    })?;
    require_stable_opened_file(path, &before, &after)?;
    Ok(bytes)
}

#[cfg(unix)]
fn require_same_opened_file(
    path: &Path,
    path_metadata: &fs::Metadata,
    opened_metadata: &fs::Metadata,
) -> Result<(), BootObjectError> {
    use std::os::unix::fs::MetadataExt;
    if path_metadata.dev() != opened_metadata.dev()
        || path_metadata.ino() != opened_metadata.ino()
        || path_metadata.file_type() != opened_metadata.file_type()
    {
        return Err(BootObjectError::UnsafePath {
            path: path.to_path_buf(),
            reason: "path identity changed between lstat and descriptor open".to_owned(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_same_opened_file(
    path: &Path,
    path_metadata: &fs::Metadata,
    opened_metadata: &fs::Metadata,
) -> Result<(), BootObjectError> {
    if path_metadata.len() != opened_metadata.len()
        || path_metadata.file_type() != opened_metadata.file_type()
    {
        return Err(BootObjectError::UnsafePath {
            path: path.to_path_buf(),
            reason: "path metadata changed between inspection and descriptor open".to_owned(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn require_stable_opened_file(
    path: &Path,
    before: &fs::Metadata,
    after: &fs::Metadata,
) -> Result<(), BootObjectError> {
    use std::os::unix::fs::MetadataExt;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(BootObjectError::UnsafePath {
            path: path.to_path_buf(),
            reason: "opened file changed during the bounded read".to_owned(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_stable_opened_file(
    path: &Path,
    before: &fs::Metadata,
    after: &fs::Metadata,
) -> Result<(), BootObjectError> {
    if before.len() != after.len() || before.file_type() != after.file_type() {
        return Err(BootObjectError::UnsafePath {
            path: path.to_path_buf(),
            reason: "opened file changed during the bounded read".to_owned(),
        });
    }
    Ok(())
}

/// Compute the canonical Git SHA-1 object identity for raw blob bytes.
pub fn git_blob_sha1(bytes: &[u8]) -> [u8; 20] {
    let header = format!("blob {}\0", bytes.len());
    let mut state = Sha1State::new();
    state.update(header.as_bytes());
    state.update(bytes);
    state.finalize()
}

struct Sha1State {
    words: [u32; 5],
    bytes: u64,
    pending: [u8; 64],
    pending_len: usize,
}

impl Sha1State {
    const fn new() -> Self {
        Self {
            words: [
                0x6745_2301,
                0xefcd_ab89,
                0x98ba_dcfe,
                0x1032_5476,
                0xc3d2_e1f0,
            ],
            bytes: 0,
            pending: [0; 64],
            pending_len: 0,
        }
    }

    fn update(&mut self, mut input: &[u8]) {
        self.bytes = self.bytes.saturating_add(input.len() as u64);
        if self.pending_len != 0 {
            let take = (64 - self.pending_len).min(input.len());
            self.pending[self.pending_len..self.pending_len + take].copy_from_slice(&input[..take]);
            self.pending_len += take;
            input = &input[take..];
            if self.pending_len < 64 {
                return;
            }
            let block = self.pending;
            self.compress(&block);
            self.pending_len = 0;
        }
        while input.len() >= 64 {
            let block: &[u8; 64] = input[..64].try_into().expect("fixed SHA-1 block");
            self.compress(block);
            input = &input[64..];
        }
        self.pending[..input.len()].copy_from_slice(input);
        self.pending_len = input.len();
    }

    fn finalize(mut self) -> [u8; 20] {
        let bit_length = self.bytes.wrapping_mul(8);
        self.update(&[0x80]);
        let zero_count = if self.pending_len <= 56 {
            56 - self.pending_len
        } else {
            64 + 56 - self.pending_len
        };
        self.update(&vec![0; zero_count]);
        self.update(&bit_length.to_be_bytes());
        debug_assert_eq!(self.pending_len, 0);
        let mut output = [0_u8; 20];
        for (chunk, word) in output.chunks_exact_mut(4).zip(self.words) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        output
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut schedule = [0_u32; 80];
        for (index, chunk) in block.chunks_exact(4).enumerate() {
            schedule[index] = u32::from_be_bytes(chunk.try_into().expect("four-byte SHA-1 word"));
        }
        for index in 16..80 {
            schedule[index] = (schedule[index - 3]
                ^ schedule[index - 8]
                ^ schedule[index - 14]
                ^ schedule[index - 16])
                .rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = self.words;
        for (index, word) in schedule.into_iter().enumerate() {
            let (function, constant) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(function)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        self.words[0] = self.words[0].wrapping_add(a);
        self.words[1] = self.words[1].wrapping_add(b);
        self.words[2] = self.words[2].wrapping_add(c);
        self.words[3] = self.words[3].wrapping_add(d);
        self.words[4] = self.words[4].wrapping_add(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn fixture() -> (BootObjectIndex, Vec<u8>, Vec<u8>) {
        let first = b"alpha\n".to_vec();
        let second = vec![0, 1, 2, 255];
        let first_sha: [u8; 32] = Sha256::digest(&first).into();
        let second_sha: [u8; 32] = Sha256::digest(&second).into();
        let index = BootObjectIndex::new(
            [0x11; 20],
            [0x22; 20],
            vec![
                BootObjectRecord::new(first_sha, git_blob_sha1(&first), first.len() as u64)
                    .unwrap(),
                BootObjectRecord::new(second_sha, git_blob_sha1(&second), second.len() as u64)
                    .unwrap(),
            ],
            vec![
                BootPathBinding::new("src/alpha.O", BootFileMode::Regular, first_sha).unwrap(),
                BootPathBinding::new("bin/alpha", BootFileMode::Executable, first_sha).unwrap(),
                BootPathBinding::new("assets/raw.bin", BootFileMode::Regular, second_sha).unwrap(),
            ],
        )
        .unwrap();
        (index, first, second)
    }

    #[test]
    fn git_blob_sha1_matches_the_standard_empty_blob_identity() {
        assert_eq!(
            hex::encode(git_blob_sha1(b"")),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
    }

    #[test]
    fn raw_sha1_matches_standard_vectors() {
        let vectors: &[(&[u8], &str)] = &[
            (b"", "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (b"abc", "a9993e364706816aba3e25717850c26c9cd0d89d"),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "84983e441c3bd26ebaae4aa1f95129e5e54670f1",
            ),
        ];
        for (input, expected) in vectors {
            let mut state = Sha1State::new();
            state.update(input);
            assert_eq!(hex::encode(state.finalize()), *expected);
        }
    }

    #[test]
    fn raw_sha1_is_invariant_under_chunking_at_block_boundaries() {
        for length in [0, 1, 7, 8, 55, 56, 63, 64, 65, 119, 120, 127, 128, 129] {
            let input: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
            let mut one_shot = Sha1State::new();
            one_shot.update(&input);
            let expected = one_shot.finalize();

            for split in 0..=input.len() {
                let mut split_state = Sha1State::new();
                split_state.update(&input[..split]);
                split_state.update(&input[split..]);
                assert_eq!(
                    split_state.finalize(),
                    expected,
                    "SHA-1 changed at length {length}, split {split}"
                );
            }

            let mut bytewise = Sha1State::new();
            for byte in &input {
                bytewise.update(std::slice::from_ref(byte));
            }
            assert_eq!(
                bytewise.finalize(),
                expected,
                "SHA-1 changed under bytewise input at length {length}"
            );
        }
    }

    #[test]
    fn git_blob_sha1_matches_git_across_sha1_block_boundaries() {
        let vectors = [
            (55, "9525d86f397cdc2c5a3672bf546c184250c8a02d"),
            (56, "ce6d37c86555d38f5461afbe7f0b0f3373519a72"),
            (63, "a10ecb02906a5b6546eb1d5564d39710f3c75599"),
            (64, "96eb299ab61d459148b19b03f71386abcec74669"),
            (65, "260cdc342c66143062d2b82ca53981b38e11101e"),
        ];
        for (length, expected) in vectors {
            let input: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
            assert_eq!(
                hex::encode(git_blob_sha1(&input)),
                expected,
                "Git blob identity changed at length {length}"
            );
        }
    }

    #[test]
    fn canonical_index_matches_the_cross_language_golden_record() {
        let data = b"x";
        let sha256: [u8; 32] = Sha256::digest(data).into();
        let index = BootObjectIndex::new(
            [0x11; 20],
            [0x22; 20],
            vec![BootObjectRecord::new(sha256, git_blob_sha1(data), 1).unwrap()],
            vec![BootPathBinding::new("x", BootFileMode::Regular, sha256).unwrap()],
        )
        .unwrap();
        let encoded = index.encode().unwrap();
        assert_eq!(encoded.len(), 211);
        assert_eq!(
            hex::encode(encoded),
            "4f424f494458000000010050000000d3111111111111111111111111111111111111111122222222222222222222222222222222222222220000000100000001000000000000000100000000000000012d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881c1b0730e0133447badcfd47fd144e254807b06e100000000000000010001000081a42d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a488178448dd2f12e496130e51201f9925f7edc46b5a0febfa19b74eaa664e2c1401e5b"
        );
    }

    #[test]
    fn canonical_index_round_trips_and_deduplicates_path_bindings() {
        let (index, first, second) = fixture();
        let encoded = index.encode().unwrap();
        let parsed = BootObjectIndex::parse(&encoded).unwrap();
        assert_eq!(parsed, index);
        assert_eq!(parsed.objects().len(), 2);
        assert_eq!(parsed.bindings().len(), 3);
        assert_eq!(parsed.stored_bytes(), (first.len() + second.len()) as u64);
        assert_eq!(
            parsed.logical_bytes(),
            (first.len() * 2 + second.len()) as u64
        );
        assert_eq!(
            encoded.len(),
            u32::from_be_bytes(encoded[12..16].try_into().unwrap()) as usize
        );
    }

    #[test]
    fn parser_rejects_root_order_path_and_reference_corruption() {
        let (index, _, _) = fixture();
        let encoded = index.encode().unwrap();

        let mut bad_root = encoded.clone();
        *bad_root.last_mut().unwrap() ^= 1;
        assert!(BootObjectIndex::parse(&bad_root)
            .unwrap_err()
            .to_string()
            .contains("root SHA-256 mismatch"));

        let mut bad_mode = encoded.clone();
        let bindings =
            BOOT_OBJECT_INDEX_HEADER_BYTES as usize + index.objects().len() * OBJECT_RECORD_BYTES;
        bad_mode[bindings + 2..bindings + 6].copy_from_slice(&0o120000_u32.to_be_bytes());
        let prefix_len = bad_mode.len() - BOOT_OBJECT_INDEX_DIGEST_BYTES;
        let digest = index_digest(&bad_mode[..prefix_len]);
        bad_mode[prefix_len..].copy_from_slice(&digest);
        assert!(BootObjectIndex::parse(&bad_mode)
            .unwrap_err()
            .to_string()
            .contains("unsupported Git file mode"));
    }

    #[test]
    fn portable_reference_is_exactly_admitted_and_authority_free() {
        let (index, _, _) = fixture();
        let object = &index.objects()[0];
        let PortableValueRecord::Extension(envelope) =
            portable_boot_object_ref(*index.root_sha256(), object).unwrap()
        else {
            panic!("boot ref must be an extension")
        };
        envelope
            .admit_exact(
                BOOT_OBJECT_REF_NAMESPACE_V1,
                BOOT_OBJECT_REF_NAME_V1,
                BOOT_OBJECT_REF_VERSION_V1,
                &boot_object_ref_schema_sha256(),
            )
            .unwrap();
        assert!(envelope
            .admit_exact(
                BOOT_OBJECT_REF_NAMESPACE_V1,
                BOOT_OBJECT_REF_NAME_V1,
                2,
                &boot_object_ref_schema_sha256(),
            )
            .is_err());
    }

    #[test]
    fn store_verifies_exact_shape_and_every_blob_identity() {
        let (index, first, second) = fixture();
        let temporary = tempdir().unwrap();
        let object_root = temporary.path().join("objects/sha256");
        fs::create_dir_all(&object_root).unwrap();
        fs::write(
            temporary.path().join(BOOT_OBJECT_INDEX_FILE),
            index.encode().unwrap(),
        )
        .unwrap();
        let first_sha: [u8; 32] = Sha256::digest(&first).into();
        for object in index.objects() {
            let chosen = if object.sha256() == &first_sha {
                first.clone()
            } else {
                second.clone()
            };
            let mut file = File::create(object_root.join(object.sha256_hex())).unwrap();
            file.write_all(&chosen).unwrap();
        }
        let store = BootObjectStore::open(temporary.path()).unwrap();
        let report = store.verify().unwrap();
        assert_eq!(report.object_count, 2);
        assert_eq!(store.read_path("src/alpha.O").unwrap(), b"alpha\n");

        let corrupt = store.object_path(store.index().bindings()[0].object_sha256());
        fs::write(corrupt, b"corrupt").unwrap();
        assert!(store.verify().is_err());
    }

    #[test]
    fn unsafe_paths_and_unreferenced_objects_fail_closed() {
        let digest: [u8; 32] = Sha256::digest(b"x").into();
        for path in ["", "/absolute", "a/../b", "a//b", "a\\b"] {
            assert!(BootPathBinding::new(path, BootFileMode::Regular, digest).is_err());
        }
        assert!(BootObjectIndex::new(
            [1; 20],
            [2; 20],
            vec![BootObjectRecord::new(digest, git_blob_sha1(b"x"), 1).unwrap()],
            vec![],
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn bounded_read_rejects_a_path_swap_between_lstat_and_open() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("object");
        let displaced = temporary.path().join("displaced");
        fs::write(&path, b"original").unwrap();
        let inspected = fs::symlink_metadata(&path).unwrap();
        fs::rename(&path, &displaced).unwrap();
        fs::write(&path, b"replaced").unwrap();
        let opened = File::open(&path).unwrap();
        assert!(read_open_file_bounded(opened, &path, &inspected, 64)
            .unwrap_err()
            .to_string()
            .contains("path identity changed"));
    }

    #[cfg(unix)]
    #[test]
    fn bounded_read_remains_pinned_if_the_path_changes_after_open() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("object");
        let displaced = temporary.path().join("displaced");
        fs::write(&path, b"original").unwrap();
        let inspected = fs::symlink_metadata(&path).unwrap();
        let opened = File::open(&path).unwrap();
        fs::rename(&path, &displaced).unwrap();
        fs::write(&path, b"replaced").unwrap();
        assert_eq!(
            read_open_file_bounded(opened, &path, &inspected, 64).unwrap(),
            b"original"
        );
    }
}
