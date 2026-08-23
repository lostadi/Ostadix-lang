use std::env;
use std::path::PathBuf;

/// Ostadix configuration root following XDG_CONFIG_HOME, with the conventional
/// macOS/Unix `$HOME/.config` fallback.
pub fn ostadix_config_root() -> PathBuf {
    if let Some(root) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(root).join("ostadix");
    }
    if let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home).join(".config").join("ostadix");
    }
    PathBuf::from(".").join(".ostadix")
}

/// Explicit/manual hosted-node configuration retained for expert operation.
pub fn hosted_config_dir() -> PathBuf {
    ostadix_config_root().join("hosted")
}

/// Usability-first automatically provisioned node identity and PKI.
pub fn lan_open_config_dir() -> PathBuf {
    ostadix_config_root().join("lan-open")
}

/// Automatically enrolled LAN peer identities.
pub fn lan_peers_config_dir() -> PathBuf {
    ostadix_config_root().join("peers")
}

/// Ostadix state root following XDG_STATE_HOME, with the conventional
/// `$HOME/.local/state` fallback.
pub fn ostadix_state_root() -> PathBuf {
    if let Some(root) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(root).join("ostadix");
    }
    if let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("ostadix");
    }
    PathBuf::from(".").join(".ostadix-state")
}

pub fn lan_open_v2_state_dir() -> PathBuf {
    ostadix_state_root().join("lan-open-v2")
}

pub fn lan_client_sessions_dir() -> PathBuf {
    ostadix_state_root().join("client-sessions")
}

pub fn lan_node_process_dir() -> PathBuf {
    ostadix_state_root().join("node")
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
