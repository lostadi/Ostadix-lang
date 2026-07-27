//! Path resolution for O toolchain (always absolute backends).

use std::path::{Path, PathBuf};

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Users/ustad"))
}

pub fn resolve_lang_root() -> PathBuf {
    if let Ok(p) = std::env::var("O_LANG_ROOT") {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            return pb;
        }
    }
    for c in [
        home_dir().join("Ostadix-lang"),
        home_dir().join("O-lang"),
        PathBuf::from("/Users/ustad/Ostadix-lang"),
    ] {
        if c.is_dir() {
            return c;
        }
    }
    home_dir().join("Ostadix-lang")
}

pub fn resolve_backends(root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("O_BACKENDS_DIR") {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            return pb;
        }
    }
    root.join("backends")
}

pub fn resolve_bin(root: &Path, name: &str, env_key: Option<&str>) -> PathBuf {
    if let Some(k) = env_key {
        if let Ok(p) = std::env::var(k) {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                return pb;
            }
        }
    }
    let release = root.join("target/release").join(name);
    if release.is_file() {
        return release;
    }
    let local = home_dir().join(".local/bin").join(name);
    if local.is_file() {
        return local;
    }
    which::which(name).unwrap_or_else(|_| PathBuf::from(name))
}

pub fn resolve_o_bin(root: &Path) -> PathBuf {
    resolve_bin(root, "O", Some("OLANG"))
}

pub fn resolve_olangc(root: &Path) -> PathBuf {
    resolve_bin(root, "olangc", None)
}

pub fn resolve_olink(root: &Path) -> PathBuf {
    resolve_bin(root, "o-link", None)
}

pub fn resolve_ounlink(root: &Path) -> PathBuf {
    resolve_bin(root, "o-unlink", None)
}

pub fn resolve_ocorec(root: &Path) -> PathBuf {
    resolve_bin(root, "ocorec", None)
}

pub fn a18_work() -> PathBuf {
    std::env::var_os("A18_WORK")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join("a18re"))
}

pub fn toolchain_env(root: &Path, backends: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("O_LANG_ROOT", root.display().to_string()),
        ("O_BACKENDS_DIR", backends.display().to_string()),
        (
            "PATH",
            format!(
                "{}:{}:{}",
                home_dir().join(".local/bin").display(),
                root.join("target/release").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        ),
    ]
}
