//! Generated-program bootstrap for an explicitly supplied runtime bundle.
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

include!("runtime_bundle/data.rs");
#[path = "linux_rootfs.rs"]
mod linux_rootfs;

pub struct RuntimeBundle(PathBuf, bool);
impl Drop for RuntimeBundle {
    fn drop(&mut self) {
        if !self.1 {
            return;
        }
        // Retained directory modes may forbid removing their children. Restore
        // owner access parent-first, including directories created by a runtime.
        // Inspect the link itself before descent so cleanup does not traverse
        // links a runtime may have left behind. Every step is best-effort: Drop
        // must preserve an activation/evaluation error already being returned.
        #[cfg(unix)]
        fn restore_owner_access(path: &std::path::Path) {
            use std::os::unix::fs::PermissionsExt;
            let Ok(metadata) = fs::symlink_metadata(path) else {
                return;
            };
            if !metadata.is_dir() {
                return;
            }
            if fs::set_permissions(
                path,
                fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
            )
            .is_err()
            {
                return;
            }
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    restore_owner_access(&entry.path());
                }
            }
        }
        #[cfg(unix)]
        restore_owner_access(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn activate() -> Result<RuntimeBundle> {
    if let Some(image) = ROOTFS_IMAGE {
        if !linux_rootfs::verify_active(image)? {
            bail!("rootfs must be entered before evaluator startup");
        }
        let guard = RuntimeBundle(PathBuf::from("/"), false);
        apply_environment(&guard.0)?;
        return Ok(guard);
    }
    let guard = extract()?;
    apply_environment(&guard.0)?;
    Ok(guard)
}

/// Runs before multicall dispatch, so even built-in backend children retain
/// the same filesystem and namespace boundary as the original program.
pub fn enter_rootfs_if_requested() -> Result<()> {
    let Some(image) = ROOTFS_IMAGE else {
        return Ok(());
    };
    if linux_rootfs::verify_active(image)? {
        apply_environment(std::path::Path::new("/"))?;
        return Ok(());
    }
    let guard = extract()?;
    let status = linux_rootfs::launch(&guard.0, image)?;
    drop(guard);
    linux_rootfs::exit_like(status)
}

fn extract() -> Result<RuntimeBundle> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)
        .map_err(|error| anyhow::anyhow!("runtime bundle entropy: {error}"))?;
    let root = std::env::temp_dir().join(format!(
        "ostadix-runtime-{}-{}",
        std::process::id(),
        hex::encode(random)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&root)?;
    }
    #[cfg(not(unix))]
    fs::create_dir(&root)?;
    let guard = RuntimeBundle(root, true);
    for (name, _) in DIRECTORIES {
        fs::create_dir_all(guard.0.join(name))?;
    }
    for (name, bytes, mode, expected) in FILES {
        if format!("{:x}", Sha256::digest(bytes)) != *expected {
            bail!("embedded runtime digest mismatch: {name}");
        }
        let dest = guard.0.join(name);
        fs::create_dir_all(dest.parent().context("runtime entry has no parent")?)?;
        fs::write(&dest, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dest, fs::Permissions::from_mode(*mode))?;
        }
    }
    // Link targets are validated against the source tree and made relative by
    // the compiler. Keep invocation aliases and canonical executable locations
    // distinct so self-relative library lookup survives relocation.
    for (name, target) in SYMLINKS {
        let dest = guard.0.join(name);
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &dest)?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(target, &dest)?;
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, dest);
            bail!("embedded runtime file symlinks are unsupported on this target");
        }
    }
    // Apply directory modes only after writing descendants. Reverse traversal
    // preserves a read-only parent's contents during extraction.
    #[cfg(unix)]
    for (name, mode) in DIRECTORIES.iter().rev() {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(guard.0.join(name), fs::Permissions::from_mode(*mode))?;
    }
    Ok(guard)
}

fn apply_environment(root: &std::path::Path) -> Result<()> {
    std::env::set_var("PATH", root.join("bin"));
    for (key, value) in ENVIRONMENT {
        let suffix = value
            .strip_prefix("${BUNDLE}/")
            .context("invalid runtime environment template")?;
        std::env::set_var(key, root.join(suffix));
    }
    Ok(())
}
