//! Safe materialization of a [`ProjectBundle`] onto disk.
//!
//! All writes are confined to the destination directory: absolute paths, `..`
//! components, and writes that would traverse an out-of-root symlink are
//! rejected. Bytes, executable bits, unix modes, empty files, and in-root
//! symlinks are restored faithfully.

use anyhow::{bail, Context, Result};
use std::path::{Component, Path, PathBuf};

use super::model::ProjectBundle;

/// A materialized workspace on disk.
#[derive(Debug)]
pub struct Workspace {
    /// The root directory the bundle was written into.
    pub root: PathBuf,
    /// Whether this workspace lives in a temporary, isolated directory.
    pub isolated: bool,
    /// When set, the root is removed when the workspace is dropped.
    cleanup: Option<PathBuf>,
}

impl Workspace {
    /// The absolute path of a project-relative path inside this workspace.
    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if let Some(dir) = &self.cleanup {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Allocate a unique, previously non-existent temp directory path.
fn unique_temp_dir() -> Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();
    for _ in 0..64 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = base.join(format!("olang-ws-{}-{}-{}", std::process::id(), nanos, n));
        if !candidate.exists() {
            std::fs::create_dir_all(&candidate)
                .with_context(|| format!("failed to create {}", candidate.display()))?;
            return Ok(candidate);
        }
    }
    bail!("could not allocate a unique workspace directory")
}

/// Validate that a stored relative path is safe (relative, no `..`, no root)
/// and return its component-joined [`PathBuf`].
fn safe_relative(path: &str) -> Result<PathBuf> {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        bail!("unsafe absolute path in bundle: {path}");
    }
    let mut out = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => bail!("unsafe `..` component in bundle path: {path}"),
            Component::Prefix(_) | Component::RootDir => {
                bail!("unsafe absolute path in bundle: {path}")
            }
        }
    }
    if out.as_os_str().is_empty() {
        bail!("empty path in bundle");
    }
    Ok(out)
}

/// Ensure that creating `target` does not traverse a symlink that escapes
/// `root`. Every existing ancestor between `root` and `target` must be a real
/// directory (not a symlink).
fn ensure_no_symlink_escape(root: &Path, target: &Path) -> Result<()> {
    let relative = target
        .strip_prefix(root)
        .with_context(|| format!("{} escapes workspace root", target.display()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        if let Ok(meta) = std::fs::symlink_metadata(&current) {
            if meta.file_type().is_symlink() && current != *target {
                bail!(
                    "refusing to write through symlink {} during materialization",
                    current.display()
                );
            }
        }
    }
    Ok(())
}

/// Materialize `bundle` into `dest_dir`, creating it if necessary.
pub fn materialize(bundle: &ProjectBundle, dest_dir: &Path) -> Result<Workspace> {
    materialize_inner(bundle, dest_dir.to_path_buf(), false, false)
}

/// Materialize `bundle` into a fresh isolated temp directory.
///
/// Each isolated workspace has a unique root, so concurrently-executed
/// alternative routes never collide on project-relative output paths. The
/// directory is removed when the returned [`Workspace`] is dropped.
pub fn materialize_isolated(bundle: &ProjectBundle) -> Result<Workspace> {
    let root = unique_temp_dir()?;
    materialize_inner(bundle, root, true, true)
}

fn materialize_inner(
    bundle: &ProjectBundle,
    dest_dir: PathBuf,
    isolated: bool,
    cleanup: bool,
) -> Result<Workspace> {
    std::fs::create_dir_all(&dest_dir)
        .with_context(|| format!("failed to create {}", dest_dir.display()))?;
    let root = dest_dir
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", dest_dir.display()))?;

    // Restore regular files first, then symlinks — so link targets exist.
    let mut symlinks = Vec::new();
    for file in &bundle.files {
        if file.is_symlink() {
            symlinks.push(file);
            continue;
        }
        let rel = safe_relative(&file.path)?;
        let target = root.join(&rel);
        ensure_no_symlink_escape(&root, &target)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&target, &file.bytes)
            .with_context(|| format!("failed to write {}", target.display()))?;
        restore_mode(&target, file.unix_mode, file.executable)?;
    }

    for file in symlinks {
        let rel = safe_relative(&file.path)?;
        let link_path = root.join(&rel);
        ensure_no_symlink_escape(&root, &link_path)?;
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let target = file
            .symlink_target
            .as_ref()
            .expect("symlink entry without target");
        // Confirm the resolved target stays within the workspace root.
        let parent = link_path.parent().unwrap_or(&root);
        let resolved = normalize(&parent.join(target));
        if !resolved.starts_with(&root) {
            bail!("symlink {} escapes workspace root", file.path);
        }
        create_symlink(target, &link_path)?;
    }

    Ok(Workspace {
        root: root.clone(),
        isolated,
        cleanup: if cleanup { Some(root) } else { None },
    })
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(unix)]
fn restore_mode(path: &Path, unix_mode: Option<u32>, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(mode) = unix_mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("failed to set mode on {}", path.display()))?;
    } else if executable {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restore_mode(_path: &Path, _unix_mode: Option<u32>, _executable: bool) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &str, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("failed to create symlink {}", link.display()))
}

#[cfg(not(unix))]
fn create_symlink(_target: &str, link: &Path) -> Result<()> {
    // On non-unix hosts, fall back to an empty placeholder file.
    std::fs::write(link, b"")
        .with_context(|| format!("failed to create placeholder for {}", link.display()))
}
