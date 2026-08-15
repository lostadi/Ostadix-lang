use std::env;
use std::path::PathBuf;

/// Ostadix configuration root following XDG_CONFIG_HOME, with the conventional
/// macOS/Unix `$HOME/.config` fallback.
pub fn hosted_config_dir() -> PathBuf {
    if let Some(root) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(root).join("ostadix").join("hosted");
    }
    if let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home)
            .join(".config")
            .join("ostadix")
            .join("hosted");
    }
    PathBuf::from(".").join(".ostadix").join("hosted")
}

pub fn default_ca_path() -> PathBuf {
    hosted_config_dir().join("ca.pem")
}

pub fn default_client_cert_path() -> PathBuf {
    hosted_config_dir().join("client-cert.pem")
}

pub fn default_client_key_path() -> PathBuf {
    hosted_config_dir().join("client-key.pem")
}

pub fn default_node_cert_path() -> PathBuf {
    hosted_config_dir().join("node-cert.pem")
}

pub fn default_node_key_path() -> PathBuf {
    hosted_config_dir().join("node-key.pem")
}
