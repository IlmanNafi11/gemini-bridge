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

fn default_concurrency_limit() -> usize {
    4
}

fn default_body_limit_bytes() -> u64 {
    10 * 1024 * 1024
}

fn default_rate_limit_capacity() -> u32 {
    60
}

fn default_rate_limit_refill_tokens() -> u32 {
    60
}

fn default_rate_limit_refill_interval_secs() -> u64 {
    60
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ServerRateLimitSetting {
    Config(RateLimitConfig),
    Enabled(bool),
}

fn deserialize_server_rate_limit<'de, D>(
    deserializer: D,
) -> Result<Option<RateLimitConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match ServerRateLimitSetting::deserialize(deserializer)? {
        ServerRateLimitSetting::Config(config) => Ok(Some(config)),
        ServerRateLimitSetting::Enabled(false) => Ok(None),
        ServerRateLimitSetting::Enabled(true) => Err(serde::de::Error::custom(
            "server.rate_limit must be a table or false",
        )),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RateLimitConfig {
    #[serde(default = "default_rate_limit_capacity")]
    pub capacity: u32,
    #[serde(default = "default_rate_limit_refill_tokens")]
    pub refill_tokens: u32,
    #[serde(default = "default_rate_limit_refill_interval_secs")]
    pub refill_interval_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            capacity: default_rate_limit_capacity(),
            refill_tokens: default_rate_limit_refill_tokens(),
            refill_interval_secs: default_rate_limit_refill_interval_secs(),
        }
    }
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
    /// Bearer API key. When set, public routes require it. Validation requires
    /// a non-empty value of at least 32 characters.
    pub api_key: Option<String>,
    /// Enable CORS responses. Must be paired with explicit `cors_origins`;
    /// wildcard origins are rejected.
    #[serde(default)]
    pub cors_enabled: bool,
    /// Explicit allowlisted CORS origins, e.g. `https://client.example`.
    /// No wildcard `*` entries are permitted.
    #[serde(default)]
    pub cors_origins: Vec<String>,
    /// Maximum number of concurrent in-flight requests (server admission).
    #[serde(default = "default_concurrency_limit")]
    pub concurrency_limit: usize,
    /// Maximum request body size in bytes for JSON API routes.
    #[serde(default = "default_body_limit_bytes")]
    pub body_limit_bytes: u64,
    /// Token-bucket rate limiting per client class. Defaults to 60 requests
    /// per minute with a burst capacity of 60; set explicitly to override.
    #[serde(
        default = "default_server_rate_limit",
        deserialize_with = "deserialize_server_rate_limit"
    )]
    pub rate_limit: Option<RateLimitConfig>,
    /// Enable local Prometheus metrics exposition. Disabled by default.
    #[serde(default)]
    pub metrics_enabled: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: default_bind_addr(),
            port: default_port(),
            api_key: None,
            cors_enabled: false,
            cors_origins: Vec::new(),
            concurrency_limit: default_concurrency_limit(),
            body_limit_bytes: default_body_limit_bytes(),
            rate_limit: default_server_rate_limit(),
            metrics_enabled: false,
        }
    }
}

fn default_server_rate_limit() -> Option<RateLimitConfig> {
    Some(RateLimitConfig::default())
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerConfig")
            .field("bind_addr", &self.bind_addr)
            .field("port", &self.port)
            .field("api_key", &self.api_key.as_ref().map(|_| "***REDACTED***"))
            .field("cors_enabled", &self.cors_enabled)
            .field("cors_origins", &self.cors_origins)
            .field("concurrency_limit", &self.concurrency_limit)
            .field("body_limit_bytes", &self.body_limit_bytes)
            .field("rate_limit", &self.rate_limit)
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
