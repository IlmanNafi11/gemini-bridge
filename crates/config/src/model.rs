use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

fn default_bind_addr() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8090
}

fn default_data_dir() -> PathBuf {
    let mut p = dirs_base();
    p.push("gemini-bridge");
    p
}

fn dirs_base() -> PathBuf {
    // XDG: $XDG_DATA_HOME or ~/.local/share
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg);
    }
    let mut home = dirs_home();
    home.push(".local");
    home.push("share");
    home
}

fn dirs_home() -> PathBuf {
    if let Ok(h) = std::env::var("HOME") {
        PathBuf::from(h)
    } else {
        PathBuf::from("~")
    }
}

fn default_media_ttl_days() -> u32 {
    30
}

fn default_tls_profile() -> String {
    "chrome".to_string()
}

fn default_timeout_secs() -> u64 {
    30
}

/// Per-server configuration.
///
/// `api_key` is intentionally excluded from `Debug` output to prevent accidental
/// credential leakage in logs.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub api_key: Option<String>,
    #[serde(default)]
    pub cors_enabled: bool,
    /// Enable local Prometheus metrics exposition. Disabled by default.
    #[serde(default)]
    pub metrics_enabled: bool,
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerConfig")
            .field("bind_addr", &self.bind_addr)
            .field("port", &self.port)
            .field("api_key", &self.api_key.as_ref().map(|_| "***REDACTED***"))
            .field("cors_enabled", &self.cors_enabled)
            .field("metrics_enabled", &self.metrics_enabled)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_media_ttl_days")]
    pub media_ttl_days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportConfig {
    #[serde(default = "default_tls_profile")]
    pub tls_profile: String,
    pub proxy_url: Option<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

/// Video generation configuration (experimental, off-by-default).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct VideoConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeConfig {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub transport: TransportConfig,
    #[serde(default)]
    pub video: VideoConfig,
}
