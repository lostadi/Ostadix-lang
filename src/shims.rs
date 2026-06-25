use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const BUNDLED_SHIMS: &[(&str, &[u8])] = &[
    ("bash_shim.py", include_bytes!("../backends/bash_shim.py")),
    (
        "common_lisp_shim.py",
        include_bytes!("../backends/common_lisp_shim.py"),
    ),
    ("cpp_shim.py", include_bytes!("../backends/cpp_shim.py")),
    (
        "csharp_shim.py",
        include_bytes!("../backends/csharp_shim.py"),
    ),
    (
        "haskell_shim.py",
        include_bytes!("../backends/haskell_shim.py"),
    ),
    ("java_shim.py", include_bytes!("../backends/java_shim.py")),
    (
        "javascript_shim.py",
        include_bytes!("../backends/javascript_shim.py"),
    ),
    ("lisp_shim.py", include_bytes!("../backends/lisp_shim.py")),
    (
        "mathematica_shim.py",
        include_bytes!("../backends/mathematica_shim.py"),
    ),
    (
        "matlab_shim.py",
        include_bytes!("../backends/matlab_shim.py"),
    ),
    ("nix_shim.py", include_bytes!("../backends/nix_shim.py")),
    (
        "nix_store_shim.py",
        include_bytes!("../backends/nix_store_shim.py"),
    ),
    (
        "nixos_test_shim.py",
        include_bytes!("../backends/nixos_test_shim.py"),
    ),
    (
        "o_shim_common.py",
        include_bytes!("../backends/o_shim_common.py"),
    ),
    ("ocaml_shim.py", include_bytes!("../backends/ocaml_shim.py")),
    (
        "python_shim.py",
        include_bytes!("../backends/python_shim.py"),
    ),
    (
        "racket_shim.py",
        include_bytes!("../backends/racket_shim.py"),
    ),
    ("ruby_shim.py", include_bytes!("../backends/ruby_shim.py")),
    ("rust_shim.py", include_bytes!("../backends/rust_shim.py")),
    ("shell_shim.py", include_bytes!("../backends/shell_shim.py")),
    ("sql_shim.py", include_bytes!("../backends/sql_shim.py")),
    (
        "webassembly_shim.py",
        include_bytes!("../backends/webassembly_shim.py"),
    ),
];

pub struct ExtractedShims {
    path: PathBuf,
}

impl ExtractedShims {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ExtractedShims {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn read_shims(override_dir: Option<&Path>) -> Result<Vec<(String, Vec<u8>)>> {
    let mut by_name: BTreeMap<String, Vec<u8>> = BUNDLED_SHIMS
        .iter()
        .map(|(name, bytes)| ((*name).to_string(), bytes.to_vec()))
        .collect();

    if let Some(dir) = override_dir {
        if !dir.exists() {
            bail!(
                "shim directory '{}' does not exist (omit the override to use bundled shims)",
                dir.display()
            );
        }
        for entry in fs::read_dir(dir)
            .with_context(|| format!("failed to read shim directory: {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let content = fs::read(&path)
                    .with_context(|| format!("failed to read shim: {}", path.display()))?;
                by_name.insert(name, content);
            }
        }
    }

    Ok(by_name.into_iter().collect())
}

pub fn extract_bundled_shims(prefix: &str) -> Result<ExtractedShims> {
    extract_shims(prefix, &read_shims(None)?)
}

pub fn extract_shims(prefix: &str, shims: &[(String, Vec<u8>)]) -> Result<ExtractedShims> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("{prefix}_{}_{}", std::process::id(), now));
    fs::create_dir_all(&dir)?;

    for (name, content) in shims {
        let dest = dir.join(name);
        fs::write(&dest, content).with_context(|| format!("failed to extract shim {name}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))
                .with_context(|| format!("failed to mark shim executable: {}", dest.display()))?;
        }
    }

    Ok(ExtractedShims { path: dir })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_shims_include_python_runtime_pair() {
        let names = BUNDLED_SHIMS
            .iter()
            .map(|(name, _)| *name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(names.contains("python_shim.py"));
        assert!(names.contains("o_shim_common.py"));
    }

    #[test]
    fn extracted_shims_are_readable_by_name() {
        let extracted = extract_bundled_shims("o_test_shims").unwrap();
        assert!(extracted.path().join("python_shim.py").is_file());
        assert!(extracted.path().join("o_shim_common.py").is_file());
    }
}
