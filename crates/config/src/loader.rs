use std::path::Path;

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};

use crate::{
    error::ConfigError,
    model::{BridgeConfig, ServerConfig, StorageConfig, TransportConfig},
};

#[allow(clippy::result_large_err)]
impl BridgeConfig {
    /// Load configuration from a TOML file, layering environment variable overrides
    /// with the `BRIDGE_` prefix on top.
    ///
    /// # Errors
    /// Returns [`ConfigError::FileNotFound`] if `path` does not exist.
    /// Returns [`ConfigError::ParseError`] on TOML/figment parse failure.
    /// Returns [`ConfigError::ValidationError`] if the resulting config is invalid.
    pub fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        Self::load_with_overrides(Some(path), None)
    }

    /// Load configuration with optional file path and optional profile name.
    ///
    /// Precedence (highest to lowest):
    /// 1. Environment variables (`BRIDGE_*`)
    /// 2. Profile overlay section in TOML (`[profile.<name>]`)
    /// 3. Base TOML sections
    /// 4. Hardcoded defaults (via `serde` default fns)
    ///
    /// # Errors
    /// Returns [`ConfigError::FileNotFound`] if a `path` is given but does not exist.
    /// Returns [`ConfigError::ParseError`] on TOML/figment parse failure.
    /// Returns [`ConfigError::ValidationError`] if the resulting config is invalid.
    pub fn load_with_overrides(
        path: Option<&Path>,
        profile: Option<&str>,
    ) -> Result<Self, ConfigError> {
        // Build defaults via serde
        let defaults = defaults_figment();

        let mut figment = defaults;

        if let Some(p) = path {
            if !p.exists() {
                return Err(ConfigError::FileNotFound(p.to_path_buf()));
            }
            // Base TOML
            figment = figment.merge(Toml::file(p));

            // Profile overlay: [profile.<name>] section merged on top of base
            if let Some(prof) = profile {
                let profile_key = format!("profile.{prof}");
                figment = figment.merge(Toml::file(p).nested().profile(prof));
                // Use figment's profile selection
                let _ = profile_key; // suppress unused warning — profile handled above
            }
        }

        // Environment variables take highest precedence
        // BRIDGE_SERVER__PORT=9000 maps to server.port
        figment = figment.merge(Env::prefixed("BRIDGE_").split("_"));

        let config: BridgeConfig = figment.extract()?;
        validate(&config)?;
        Ok(config)
    }
}

/// Build a Figment seeded with all hardcoded defaults.
fn defaults_figment() -> Figment {
    // Construct a default BridgeConfig and serialize it to a toml string so
    // figment can use it as the lowest-priority provider.
    let default_server = ServerConfig {
        bind_addr: "127.0.0.1".to_string(),
        port: 8090,
        api_key: None,
        cors_enabled: false,
    };
    let default_storage = StorageConfig {
        data_dir: xdg_data_dir(),
        media_ttl_days: 30,
    };
    let default_transport = TransportConfig {
        tls_profile: "chrome".to_string(),
        proxy_url: None,
        timeout_secs: 30,
    };
    let default_config = BridgeConfig {
        server: default_server,
        storage: default_storage,
        transport: default_transport,
        video: crate::model::VideoConfig::default(),
    };

    // Serialize to TOML and inject as a Toml string provider
    let toml_str = toml::to_string(&default_config).expect("default config is always serializable");
    Figment::new().merge(Toml::string(&toml_str))
}

fn xdg_data_dir() -> std::path::PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        let mut p = std::path::PathBuf::from(xdg);
        p.push("gemini-bridge");
        return p;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
    let mut p = std::path::PathBuf::from(home);
    p.push(".local");
    p.push("share");
    p.push("gemini-bridge");
    p
}

#[allow(clippy::result_large_err)]
fn validate(config: &BridgeConfig) -> Result<(), ConfigError> {
    if config.server.port == 0 {
        return Err(ConfigError::ValidationError(
            "server.port must not be 0".to_string(),
        ));
    }
    Ok(())
}
