//! Lossless project bundling: walk a directory into a [`ProjectBundle`] and
//! (de)serialize it deterministically.
//!
//! The walk mirrors o-link's collection rules — respecting `.gitignore` and
//! `.olinkignore`, skipping `.git` and other build/output directories — but,
//! unlike o-link, it is *lossless*: binary assets, empty and extensionless
//! files, executable bits, unix modes, and in-root symlinks are all captured.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use super::model::{
    FileRole, ProjectBundle, ProjectFile, BUNDLE_FORMAT_VERSION,
};

/// Directories that are never captured (build outputs, VCS metadata, caches).
const SKIP_DIRS: &[&str] = &["target", "node_modules", "__pycache__", ".git"];

/// Compute the lowercase hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Bundle the directory rooted at `root` into a [`ProjectBundle`].
///
/// The resulting `files` list is sorted by path, every path is normalized to a
/// relative, `/`-separated string, and `root_fingerprint` is a deterministic
/// hash of the `(path, content_hash, mode)` triples.
pub fn bundle_dir(root: &Path, name: &str) -> Result<ProjectBundle> {
    if !root.is_dir() {
        bail!("{}: not a directory", root.display());
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", root.display()))?;

    let mut files: Vec<ProjectFile> = Vec::new();
    let mut ignore_stack: Vec<IgnoreRules> = Vec::new();
    walk(&root, &root, &mut ignore_stack, &mut files)?;

    files.sort_by(|a, b| a.path.cmp(&b.path));

    let root_fingerprint = fingerprint(&files);

    Ok(ProjectBundle {
        format_version: BUNDLE_FORMAT_VERSION,
        name: name.to_string(),
        root_fingerprint,
        files,
        routes: Vec::new(),
        route_sets: Vec::new(),
        default_route: None,
        metadata: BTreeMap::new(),
    })
}

/// Deterministic fingerprint of a sorted file list: sha256 over
/// `path\0content_hash\0mode\n` lines.
pub fn fingerprint(files: &[ProjectFile]) -> String {
    let mut hasher = Sha256::new();
    for file in files {
        hasher.update(file.path.as_bytes());
        hasher.update([0]);
        hasher.update(file.content_hash.as_bytes());
        hasher.update([0]);
        hasher.update(file.unix_mode.unwrap_or(0).to_le_bytes());
        hasher.update([0]);
        hasher.update([file.symlink_target.is_some() as u8]);
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

struct IgnoreRules {
    matcher: ignore::gitignore::Gitignore,
}

fn load_ignore_rules(dir: &Path, stack: &mut Vec<IgnoreRules>) -> usize {
    let start = stack.len();
    for name in [".gitignore", ".olinkignore"] {
        let source = dir.join(name);
        if !source.is_file() {
            continue;
        }
        let mut builder = ignore::gitignore::GitignoreBuilder::new(dir);
        if builder.add(&source).is_some() {
            continue;
        }
        if let Ok(matcher) = builder.build() {
            stack.push(IgnoreRules { matcher });
        }
    }
    start
}

fn ignored(stack: &[IgnoreRules], path: &Path, is_dir: bool) -> bool {
    let mut ignored = false;
    for rules in stack {
        let matched = rules.matcher.matched(path, is_dir);
        if matched.is_ignore() {
            ignored = true;
        } else if matched.is_whitelist() {
            ignored = false;
        }
    }
    ignored
}

fn walk(
    root: &Path,
    dir: &Path,
    ignore_stack: &mut Vec<IgnoreRules>,
    out: &mut Vec<ProjectFile>,
) -> Result<()> {
    let added_from = load_ignore_rules(dir, ignore_stack);

    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();

    for entry in entries {
        let name = entry
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let meta = match std::fs::symlink_metadata(&entry) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let file_type = meta.file_type();
        let is_dir = file_type.is_dir();

        if ignored(ignore_stack, &entry, is_dir) {
            continue;
        }

        if file_type.is_symlink() {
            capture_symlink(root, &entry, out)?;
            continue;
        }

        if is_dir {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(root, &entry, ignore_stack, out)?;
            continue;
        }

        if file_type.is_file() {
            capture_file(root, &entry, &meta, out)?;
        }
    }

    ignore_stack.truncate(added_from);
    Ok(())
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("{} escapes project root", path.display()))?;
    if relative.as_os_str().is_empty() {
        bail!("empty relative path for {}", path.display());
    }
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                let s = part
                    .to_str()
                    .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
                parts.push(s.to_string());
            }
            _ => bail!("unsafe path component in {}", path.display()),
        }
    }
    Ok(parts.join("/"))
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.mode())
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> Option<u32> {
    None
}

fn capture_file(
    root: &Path,
    path: &Path,
    meta: &std::fs::Metadata,
    out: &mut Vec<ProjectFile>,
) -> Result<()> {
    let rel = relative_path(root, path)?;
    let bytes = std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let unix_mode = mode_of(meta);
    let executable = unix_mode.map(|m| m & 0o111 != 0).unwrap_or(false);
    let content_hash = sha256_hex(&bytes);
    let role = classify_role(&rel, &bytes, executable);
    let evaluator = infer_evaluator(&rel);

    out.push(ProjectFile {
        path: rel,
        bytes,
        executable,
        unix_mode,
        symlink_target: None,
        evaluator,
        content_hash,
        role,
    });
    Ok(())
}

fn capture_symlink(root: &Path, path: &Path, out: &mut Vec<ProjectFile>) -> Result<()> {
    let rel = relative_path(root, path)?;
    let target = std::fs::read_link(path)
        .with_context(|| format!("failed to read symlink {}", path.display()))?;

    // Resolve the target relative to the link's parent and reject anything that
    // escapes the project root.
    let parent = path.parent().unwrap_or(root);
    let resolved = if target.is_absolute() {
        target.clone()
    } else {
        normalize(&parent.join(&target))
    };
    if !resolved.starts_with(root) {
        eprintln!(
            "warning: skipping symlink {} → {} (escapes project root)",
            path.display(),
            target.display()
        );
        return Ok(());
    }

    let target_str = target
        .to_str()
        .with_context(|| format!("symlink target for {} is not UTF-8", path.display()))?
        .replace('\\', "/");
    let content_hash = sha256_hex(target_str.as_bytes());

    out.push(ProjectFile {
        path: rel,
        bytes: Vec::new(),
        executable: false,
        unix_mode: None,
        symlink_target: Some(target_str),
        evaluator: None,
        content_hash,
        role: FileRole::Other,
    });
    Ok(())
}

/// Normalize a path lexically (resolving `.` and `..` without touching disk).
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

fn infer_evaluator(rel: &str) -> Option<String> {
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    let backend = match ext.as_str() {
        "py" => "python",
        "sh" | "bash" => "bash",
        "html" | "htm" => "html",
        "md" | "markdown" => "markdown",
        "rs" => "rust",
        "js" | "mjs" | "cjs" => "javascript",
        "rb" => "ruby",
        "java" => "java",
        "c" => "c",
        "cc" | "cpp" | "cxx" => "cpp",
        "hs" => "haskell",
        "ml" => "ocaml",
        "nix" => "nix",
        "sql" => "sql",
        "tex" => "latex",
        _ => return None,
    };
    Some(backend.to_string())
}

fn classify_role(rel: &str, bytes: &[u8], executable: bool) -> FileRole {
    let name = Path::new(rel)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let manifests = [
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "Makefile",
        "makefile",
        "olang.project.toml",
        "o.toml",
        "go.mod",
        "pom.xml",
        "build.gradle",
    ];
    if manifests.contains(&name) {
        return FileRole::Manifest;
    }
    let is_text = std::str::from_utf8(bytes).is_ok();
    if !is_text && !bytes.is_empty() {
        return FileRole::Asset;
    }
    if executable {
        return FileRole::Entrypoint;
    }
    if infer_evaluator(rel).is_some() {
        return FileRole::Source;
    }
    FileRole::Other
}

// ─────────────────────────────────────────────────────────────────────────────
// Serialization
// ─────────────────────────────────────────────────────────────────────────────

/// Serialize a bundle to canonical, deterministic JSON bytes.
///
/// `serde_json` with the workspace's `preserve_order` feature preserves struct
/// field order, and all keyed collections in the model are `BTreeMap`, so the
/// output is stable across runs.
pub fn serialize(bundle: &ProjectBundle) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(bundle).context("failed to serialize project bundle")?;
    Ok(bytes)
}

/// Serialize a bundle to a pretty JSON string (for human inspection).
pub fn serialize_pretty(bundle: &ProjectBundle) -> Result<String> {
    serde_json::to_string_pretty(bundle).context("failed to serialize project bundle")
}

/// Deserialize a bundle from JSON bytes.
pub fn deserialize(bytes: &[u8]) -> Result<ProjectBundle> {
    let bundle: ProjectBundle =
        serde_json::from_slice(bytes).context("failed to deserialize project bundle")?;
    if bundle.format_version != BUNDLE_FORMAT_VERSION {
        bail!(
            "unsupported project bundle format version {} (expected {})",
            bundle.format_version,
            BUNDLE_FORMAT_VERSION
        );
    }
    Ok(bundle)
}
