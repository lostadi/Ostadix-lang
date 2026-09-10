//! Embed an explicitly supplied relocatable foreign-runtime tree.
//!
//! This closes command lookup over bundled `bin/` entries. It does not infer
//! dynamic-library closure or turn daemon-backed runtimes into local ones.
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

const BOOTSTRAP: &str = include_str!("embedded_runtime.rs");
const LINUX_ROOTFS: &str = include_str!("linux_rootfs.rs");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    #[serde(default)]
    execution: Option<String>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
}

fn relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn relocated_link_target(root: &Path, path: &Path, resolved: &Path) -> Result<String> {
    let original = fs::read_link(path)?;
    let parent = path.parent().context("bundle link has no parent")?;
    let parent_depth = parent.strip_prefix(root)?.components().count();
    let target = if original.is_absolute() {
        let mut target = PathBuf::new();
        for _ in 0..parent_depth {
            target.push("..");
        }
        target.push(resolved.strip_prefix(root)?);
        target
    } else {
        // A target that leaves and then re-enters the source bundle would
        // depend on the source parent's layout after extraction. Reject it.
        let mut depth = parent_depth;
        for component in original.components() {
            match component {
                Component::Normal(_) => depth += 1,
                Component::CurDir => {}
                Component::ParentDir if depth > 0 => depth -= 1,
                _ => bail!("bundle link target escapes its root: {}", path.display()),
            }
        }
        original
    };
    Ok(target
        .to_str()
        .context("bundle link targets must be UTF-8")?
        .to_owned())
}

pub(super) fn embed(root: &Path, build_dir: &Path) -> Result<()> {
    let root = root.canonicalize().context("resolve runtime bundle")?;
    let manifest: Manifest = serde_json::from_slice(&fs::read(root.join("runtime.json"))?)?;
    if manifest.schema != "ostadix.embedded-runtime/v1" {
        bail!("unsupported embedded runtime manifest schema");
    }
    let rootfs = match manifest.execution.as_deref() {
        None | Some("host") => false,
        Some("linux-rootfs-v1") => true,
        Some(other) => bail!("unsupported embedded execution profile {other}"),
    };
    if rootfs {
        for reserved in [".ostadix", ".old-root", "proc", "dev", "tmp", "work", "run"] {
            if fs::symlink_metadata(root.join(reserved)).is_ok() {
                bail!("reserved rootfs execution path: {reserved}");
            }
        }
    }
    if !root.join("bin").is_dir() {
        bail!("runtime bundle requires bin/");
    }
    for (key, value) in &manifest.environment {
        if !matches!(
            key.as_str(),
            "PYTHONHOME"
                | "PYTHONPATH"
                | "NODE_PATH"
                | "RUBYLIB"
                | "GEM_HOME"
                | "GEM_PATH"
                | "JAVA_HOME"
                | "DOTNET_ROOT"
                | "LD_LIBRARY_PATH"
                | "DYLD_LIBRARY_PATH"
        ) {
            bail!("unsupported runtime environment key {key}");
        }
        let suffix = value
            .strip_prefix("${BUNDLE}/")
            .context("runtime environment values must start with ${BUNDLE}/")?;
        if !relative(Path::new(suffix)) || suffix.contains(['\0', ':']) {
            bail!("runtime environment path must remain inside bundle");
        }
        let resolved = root.join(suffix).canonicalize()?;
        if !resolved.starts_with(&root) {
            bail!("runtime environment escapes bundle");
        }
    }

    fn mode(metadata: &fs::Metadata) -> u32 {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o777
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            0o755
        }
    }
    fn walk(
        root: &Path,
        dir: &Path,
        entries: &mut Vec<(String, Vec<u8>, u32)>,
        directories: &mut Vec<(String, u32)>,
        symlinks: &mut Vec<(String, String, u32)>,
    ) -> Result<()> {
        let mut children = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let path = child.path();
            let metadata = fs::symlink_metadata(&path)?;
            let name = path
                .strip_prefix(root)?
                .to_str()
                .context("bundle paths must be UTF-8")?
                .to_owned();
            if !relative(Path::new(&name)) {
                bail!("invalid bundle path {name}");
            }
            if metadata.is_dir() {
                directories.push((name, mode(&metadata)));
                walk(root, &path, entries, directories, symlinks)?;
            } else {
                let resolved = path.canonicalize()?;
                if !resolved.starts_with(root) {
                    bail!("bundle link escapes its root: {name}");
                }
                let target = fs::metadata(&resolved)?;
                if !target.is_file() {
                    bail!("bundle accepts regular files and internal file links only: {name}");
                }
                if metadata.file_type().is_symlink() {
                    symlinks.push((
                        name,
                        relocated_link_target(root, &path, &resolved)?,
                        mode(&target),
                    ));
                } else {
                    entries.push((name, fs::read(&resolved)?, mode(&target)));
                }
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    let mut directories = Vec::new();
    let mut symlinks = Vec::new();
    walk(&root, &root, &mut entries, &mut directories, &mut symlinks)?;
    if !entries
        .iter()
        .any(|(name, _, mode)| name.starts_with("bin/") && mode & 0o111 != 0)
        && !symlinks
            .iter()
            .any(|(name, _, mode)| name.starts_with("bin/") && mode & 0o111 != 0)
    {
        bail!("runtime bundle has no executable bin entry");
    }
    let bundle_dir = build_dir.join("src/runtime_bundle");
    fs::create_dir_all(&bundle_dir)?;
    let mut data = String::from("const FILES: &[(&str, &[u8], u32, &str)] = &[\n");
    let mut inventory = Vec::new();
    for (index, (name, bytes, mode)) in entries.iter().enumerate() {
        let file = format!("payload-{index}");
        fs::write(bundle_dir.join(&file), bytes)?;
        let digest = format!("{:x}", Sha256::digest(bytes));
        data.push_str(&format!(
            "({name:?}, include_bytes!({file:?}), {mode}, {digest:?}),\n"
        ));
        inventory
            .push(serde_json::json!({"path":name,"sha256":digest,"bytes":bytes.len(),"mode":mode}));
    }
    data.push_str("];\nconst DIRECTORIES: &[(&str, u32)] = &[\n");
    for (name, mode) in &directories {
        data.push_str(&format!("({name:?}, {mode}),\n"));
    }
    data.push_str("];\nconst SYMLINKS: &[(&str, &str)] = &[\n");
    for (name, target, _) in &symlinks {
        data.push_str(&format!("({name:?}, {target:?}),\n"));
    }
    data.push_str("];\nconst ENVIRONMENT: &[(&str, &str)] = &[\n");
    for (key, value) in &manifest.environment {
        data.push_str(&format!("({key:?}, {value:?}),\n"));
    }
    data.push_str("];\n");
    let inventory = serde_json::json!({
        "schema":"ostadix.embedded-runtime-inventory/v1", "command_lookup":"bundle-only",
        "execution": if rootfs { "linux-rootfs-v1" } else { "host" },
        "dynamic_closure_verified":false, "environment":manifest.environment,
        "directories":directories.iter().map(|(path, mode)| serde_json::json!({"path":path,"mode":mode})).collect::<Vec<_>>(),
        "symlinks":symlinks.iter().map(|(path, target, mode)| serde_json::json!({"path":path,"target":target,"target_mode":mode})).collect::<Vec<_>>(),
        "files":inventory
    });
    let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&inventory)?));
    data.push_str(&format!(
        "const ROOTFS_IMAGE: Option<&str> = {};\n",
        if rootfs {
            format!("Some({digest:?})")
        } else {
            "None".into()
        }
    ));
    fs::write(bundle_dir.join("data.rs"), data)?;
    fs::write(build_dir.join("src/embedded_runtime.rs"), BOOTSTRAP)?;
    fs::write(build_dir.join("src/linux_rootfs.rs"), LINUX_ROOTFS)?;
    fs::write(
        build_dir.join("runtime-bundle-manifest.json"),
        serde_json::to_vec_pretty(&inventory)?,
    )?;
    let main_path = build_dir.join("src/main.rs");
    let mut main = fs::read_to_string(&main_path)?;
    if rootfs {
        let entry = "fn main() -> anyhow::Result<()> {";
        if main.matches(entry).count() != 1 {
            bail!("generated rootfs entry point is missing or ambiguous");
        }
        main = main.replacen(
            entry,
            &format!("{entry}\n    embedded_runtime::enter_rootfs_if_requested()?;"),
            1,
        );
    }
    let marker = "    #[cfg(not(target_family = \"wasm\"))]\n    let shim_dir =";
    if main.matches(marker).count() != 1 {
        bail!("generated runtime bootstrap insertion point is missing or ambiguous");
    }
    let main = main.replacen(
        marker,
        &format!("    let _runtime_bundle = embedded_runtime::activate()?;\n\n{marker}"),
        1,
    );
    fs::write(main_path, format!("mod embedded_runtime;\n{main}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("bin")).unwrap();
        fs::write(
            root.path().join("runtime.json"),
            br#"{"schema":"ostadix.embedded-runtime/v1"}"#,
        )
        .unwrap();
        fs::write(root.path().join("bin/example"), b"runtime payload").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                root.path().join("bin/example"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        root
    }
    #[test]
    fn rootfs_gate_precedes_multicall_and_binds_the_image_inventory() {
        let root = fixture();
        fs::write(
            root.path().join("runtime.json"),
            br#"{"schema":"ostadix.embedded-runtime/v1","execution":"linux-rootfs-v1"}"#,
        )
        .unwrap();
        let out = tempfile::tempdir().unwrap();
        fs::create_dir(out.path().join("src")).unwrap();
        fs::write(out.path().join("src/main.rs"), "fn main() -> anyhow::Result<()> {\n    backend::run_backend_from_env_args()?;\n    #[cfg(not(target_family = \"wasm\"))]\n    let shim_dir = foo();\n    Ok(())\n}").unwrap();
        embed(root.path(), out.path()).unwrap();
        let main = fs::read_to_string(out.path().join("src/main.rs")).unwrap();
        assert!(
            main.find("enter_rootfs_if_requested").unwrap()
                < main.find("run_backend_from_env_args").unwrap()
        );
        let inventory = fs::read(out.path().join("runtime-bundle-manifest.json")).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&inventory).unwrap();
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()));
        assert!(
            fs::read_to_string(out.path().join("src/runtime_bundle/data.rs"))
                .unwrap()
                .contains(&format!("Some({digest:?})"))
        );
        assert!(out.path().join("src/linux_rootfs.rs").is_file());
        fs::create_dir(root.path().join("proc")).unwrap();
        assert!(embed(root.path(), out.path())
            .unwrap_err()
            .to_string()
            .contains("reserved rootfs"));
    }
    #[test]
    fn embeds_bytes_inventory_and_closed_command_search() {
        let root = fixture();
        fs::create_dir_all(root.path().join("lib/empty")).unwrap();
        let out = tempfile::tempdir().unwrap();
        fs::create_dir(out.path().join("src")).unwrap();
        fs::write(
            out.path().join("src/main.rs"),
            "    #[cfg(not(target_family = \"wasm\"))]\n    let shim_dir = foo();",
        )
        .unwrap();
        embed(root.path(), out.path()).unwrap();
        let inventory: serde_json::Value = serde_json::from_slice(
            &fs::read(out.path().join("runtime-bundle-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(inventory["command_lookup"], "bundle-only");
        assert!(inventory["directories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dir| dir["path"] == "lib/empty"));
        assert_eq!(
            inventory["files"][0]["sha256"],
            format!("{:x}", Sha256::digest(b"runtime payload"))
        );
        assert!(fs::read_to_string(out.path().join("src/main.rs"))
            .unwrap()
            .contains("embedded_runtime::activate()?"));
    }
    #[cfg(unix)]
    #[test]
    fn refuses_external_symlinks_and_environment_escape() {
        use std::os::unix::fs::symlink;
        let root = fixture();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), root.path().join("bin/escape")).unwrap();
        let out = tempfile::tempdir().unwrap();
        assert!(embed(root.path(), out.path())
            .unwrap_err()
            .to_string()
            .contains("escapes"));
        fs::remove_file(root.path().join("bin/escape")).unwrap();
        fs::write(root.path().join("runtime.json"), br#"{"schema":"ostadix.embedded-runtime/v1","environment":{"PYTHONHOME":"${BUNDLE}/../outside"}}"#).unwrap();
        assert!(embed(root.path(), out.path())
            .unwrap_err()
            .to_string()
            .contains("inside bundle"));
    }

    #[cfg(unix)]
    #[test]
    fn preserves_relative_file_links_and_relocates_absolute_targets() {
        use std::os::unix::fs::symlink;
        let root = fixture();
        fs::create_dir(root.path().join("libexec")).unwrap();
        fs::rename(
            root.path().join("bin/example"),
            root.path().join("libexec/example"),
        )
        .unwrap();
        symlink("../libexec/./example", root.path().join("bin/example")).unwrap();
        symlink("example", root.path().join("bin/chain")).unwrap();
        symlink(
            root.path().join("libexec/example").canonicalize().unwrap(),
            root.path().join("bin/absolute"),
        )
        .unwrap();
        let out = tempfile::tempdir().unwrap();
        fs::create_dir(out.path().join("src")).unwrap();
        fs::write(
            out.path().join("src/main.rs"),
            "    #[cfg(not(target_family = \"wasm\"))]\n    let shim_dir = foo();",
        )
        .unwrap();
        embed(root.path(), out.path()).unwrap();
        let inventory: serde_json::Value = serde_json::from_slice(
            &fs::read(out.path().join("runtime-bundle-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            inventory["symlinks"],
            serde_json::json!([
                {"path":"bin/absolute", "target":"../libexec/example", "target_mode":0o755},
                {"path":"bin/chain", "target":"example", "target_mode":0o755},
                {"path":"bin/example", "target":"../libexec/./example", "target_mode":0o755}
            ])
        );
        assert!(!inventory["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"].as_str().unwrap().starts_with("bin/")));
        assert!(
            fs::read_to_string(out.path().join("src/runtime_bundle/data.rs"))
                .unwrap()
                .contains("const SYMLINKS:")
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_directory_links_and_relative_targets_that_leave_then_reenter_root() {
        use std::os::unix::fs::symlink;
        let root = fixture();
        let out = tempfile::tempdir().unwrap();
        symlink("bin", root.path().join("directory-link")).unwrap();
        assert!(embed(root.path(), out.path())
            .unwrap_err()
            .to_string()
            .contains("internal file links only"));
        fs::remove_file(root.path().join("directory-link")).unwrap();
        let root_name = root.path().file_name().unwrap().to_str().unwrap();
        symlink(
            format!("../../{root_name}/bin/example"),
            root.path().join("bin/reenter"),
        )
        .unwrap();
        assert!(embed(root.path(), out.path())
            .unwrap_err()
            .to_string()
            .contains("target escapes"));
    }
}
