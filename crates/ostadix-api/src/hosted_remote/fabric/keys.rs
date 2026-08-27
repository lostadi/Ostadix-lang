//! Private on-disk key material for the opt-in Fabric provider.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use anyhow::{bail, Context, Result};

use crate::execution_fabric_authority::FabricSigningKeyV1;

const FABRIC_NODE_KEY_MAGIC_V1: &[u8] = b"OSTADIX-FABRIC-NODE-KEY-V1\0";
const MAX_KEY_FILE_BYTES: u64 = 1024;

pub fn write_new_fabric_node_signing_key_v1(
    path: impl AsRef<Path>,
    signer: &FabricSigningKeyV1,
) -> Result<()> {
    let path = path.as_ref();
    let parent = usable_parent(path);
    ensure_private_directory(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let mut file = options
        .open(path)
        .with_context(|| format!("refusing to overwrite Fabric node key `{}`", path.display()))?;
    file.write_all(FABRIC_NODE_KEY_MAGIC_V1)?;
    file.write_all(&signer.secret_bytes())?;
    file.sync_all()?;
    sync_directory(parent)
}

pub fn read_fabric_node_signing_key_v1(path: impl AsRef<Path>) -> Result<FabricSigningKeyV1> {
    let path = path.as_ref();
    require_regular_file(path, "Fabric node key")?;
    let mut file = open_file_no_follow(path, "Fabric node key")?;
    #[cfg(unix)]
    if file.metadata()?.permissions().mode() & 0o077 != 0 {
        bail!(
            "Fabric node key `{}` must not be accessible by group or other users",
            path.display()
        );
    }
    let bytes = read_small_file(&mut file, "Fabric node key")?;
    if bytes.len() != FABRIC_NODE_KEY_MAGIC_V1.len() + 32
        || !bytes.starts_with(FABRIC_NODE_KEY_MAGIC_V1)
    {
        bail!("Fabric node signing key has an invalid V1 encoding");
    }
    let mut secret = [0_u8; 32];
    secret.copy_from_slice(&bytes[FABRIC_NODE_KEY_MAGIC_V1.len()..]);
    Ok(FabricSigningKeyV1::from_secret_bytes(secret))
}

pub fn write_new_fabric_public_key_v1(path: impl AsRef<Path>, public_key: &[u8; 32]) -> Result<()> {
    let path = path.as_ref();
    let parent = usable_parent(path);
    ensure_private_directory(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path).with_context(|| {
        format!(
            "refusing to overwrite Fabric public key `{}`",
            path.display()
        )
    })?;
    writeln!(file, "{}", hex::encode(public_key))?;
    file.sync_all()?;
    sync_directory(parent)
}

pub fn read_fabric_public_key_v1(path: impl AsRef<Path>) -> Result<[u8; 32]> {
    let path = path.as_ref();
    require_regular_file(path, "Fabric public key")?;
    let mut file = open_file_no_follow(path, "Fabric public key")?;
    let bytes = read_small_file(&mut file, "Fabric public key")?;
    let text = std::str::from_utf8(&bytes).context("Fabric public key is not UTF-8")?;
    let canonical = text.trim_end_matches('\n');
    if canonical.len() != 64
        || canonical
            .bytes()
            .any(|byte| !(byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        || (text != canonical && text != format!("{canonical}\n"))
    {
        bail!("Fabric public key must be exactly 64 lowercase hexadecimal characters");
    }
    let mut public_key = [0_u8; 32];
    hex::decode_to_slice(canonical, &mut public_key)
        .context("Fabric public key is not hexadecimal")?;
    Ok(public_key)
}

fn require_regular_file(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} `{}`", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} must be a regular, non-symlink file");
    }
    Ok(())
}

fn open_file_no_follow(path: &Path, label: &str) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    options
        .open(path)
        .with_context(|| format!("failed to open {label} `{}`", path.display()))
}

fn read_small_file(file: &mut File, label: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(MAX_KEY_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_KEY_FILE_BYTES {
        bail!("{label} exceeds the bounded key-file size");
    }
    Ok(bytes)
}

fn usable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "Fabric key directory `{}` must be a real directory",
                    path.display()
                );
            }
            #[cfg(unix)]
            if metadata.permissions().mode() & 0o077 != 0 {
                bail!(
                    "Fabric key directory `{}` must have mode 0700",
                    path.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(path).with_context(|| {
                format!("failed to create Fabric key directory `{}`", path.display())
            })?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open directory `{}` for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync directory `{}`", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_and_public_keys_round_trip_without_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let node_path = directory.path().join("node-key.v1");
        let public_path = directory.path().join("authority.pub");
        let signer = FabricSigningKeyV1::from_secret_bytes([0x42; 32]);

        write_new_fabric_node_signing_key_v1(&node_path, &signer).unwrap();
        assert_eq!(
            read_fabric_node_signing_key_v1(&node_path)
                .unwrap()
                .public_key(),
            signer.public_key()
        );
        assert!(write_new_fabric_node_signing_key_v1(&node_path, &signer).is_err());

        write_new_fabric_public_key_v1(&public_path, &signer.public_key()).unwrap();
        assert_eq!(
            read_fabric_public_key_v1(&public_path).unwrap(),
            signer.public_key()
        );
        assert!(write_new_fabric_public_key_v1(&public_path, &signer.public_key()).is_err());
    }
}
