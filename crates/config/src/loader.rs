use std::{collections::HashSet, path::Path};

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use toml::{Table, Value};

use crate::{
    error::ConfigError,
    model::{BridgeConfig, ServerConfig, StorageConfig, TransportConfig},
    overlay,
    profiles::{BundleDefinition, Composition},
};

#[allow(clippy::result_large_err)]
impl BridgeConfig {
    /// Load configuration from a TOML file, layering `BRIDGE_*` environment
    /// variable overrides on top.
    pub fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        Self::load_composed(Some(path), &Composition::new())
    }

    /// Load configuration with an optional legacy single-profile selection.
    ///
    /// New callers that need bundles or patches should use [`Self::load_composed`].
    pub fn load_with_overrides(
        path: Option<&Path>,
        profile: Option<&str>,
    ) -> Result<Self, ConfigError> {
        if path.is_none() {
            return Self::load_composed(None, &Composition::new());
        }
        let composition = profile
            .map(|name| Composition::new().with_profile(name))
            .unwrap_or_default();
        Self::load_composed(path, &composition)
    }

    /// Load a deterministic composition of named bundles, profiles, and patches.
    ///
    /// Precedence (lowest to highest) is hardcoded defaults, base TOML, profiles
    /// selected by bundles, explicitly selected profiles, patches selected by
    /// bundles, explicitly selected patches, then `BRIDGE_*` environment values.
    pub fn load_composed(
        path: Option<&Path>,
        composition: &Composition,
    ) -> Result<Self, ConfigError> {
        let mut document = load_document(path)?;
        let root = document.as_table_mut().ok_or_else(|| {
            ConfigError::CompositionError("configuration root must be a TOML table".to_string())
        })?;

        let profiles = take_profiles(root)?;
        let patches = take_named_tables(root, "patches")?;
        let bundles = take_bundles(root)?;

        reject_duplicates("bundle", &composition.bundles)?;
        reject_duplicates("profile", &composition.profiles)?;
        reject_duplicates("patch", &composition.patches)?;

        let mut bundle_profiles = Vec::new();
        let mut bundle_patches = Vec::new();
        for bundle_name in &composition.bundles {
            let bundle = bundles.get(bundle_name).ok_or_else(|| {
                ConfigError::CompositionError(format!("unknown bundle '{bundle_name}'"))
            })?;
            for profile in &bundle.profiles {
                if !profiles.contains_key(profile) {
                    return Err(ConfigError::CompositionError(format!(
                        "bundle '{bundle_name}' references unknown profile '{profile}'"
                    )));
                }
                bundle_profiles.push(profile.clone());
            }
            for patch in &bundle.patches {
                if !patches.contains_key(patch) {
                    return Err(ConfigError::CompositionError(format!(
                        "bundle '{bundle_name}' references unknown patch '{patch}'"
                    )));
                }
                bundle_patches.push(patch.clone());
            }
        }

        let selected_profiles = combine_names(&bundle_profiles, &composition.profiles);
        let selected_patches = combine_names(&bundle_patches, &composition.patches);
        reject_duplicates("profile", &selected_profiles)?;
        reject_duplicates("patch", &selected_patches)?;
        reject_conflicting_profiles(&selected_profiles)?;

        for profile in selected_profiles {
            apply_named_overlay(&mut document, &profiles, "profile", &profile)?;
        }
        for patch in selected_patches {
            apply_named_overlay(&mut document, &patches, "patch", &patch)?;
        }

        let composed = toml::to_string(&document).map_err(|error| {
            ConfigError::CompositionError(format!("could not serialize composed config: {error}"))
        })?;
        let figment = defaults_figment()
            .merge(Toml::string(&composed))
            .merge(Env::prefixed("BRIDGE_").split("_"));
        let config: BridgeConfig = figment.extract()?;
        validate(&config)?;
        Ok(config)
    }
}

fn load_document(path: Option<&Path>) -> Result<Value, ConfigError> {
    let Some(path) = path else {
        return Ok(Value::Table(Table::new()));
    };
    if !path.exists() {
        return Err(ConfigError::FileNotFound(path.to_path_buf()));
    }
    let source = std::fs::read_to_string(path).map_err(|error| {
        ConfigError::CompositionError(format!("could not read '{}': {error}", path.display()))
    })?;
    Figment::new()
        .merge(Toml::string(&source))
        .extract()
        .map_err(Into::into)
}

fn take_profiles(root: &mut Table) -> Result<Table, ConfigError> {
    let profiles = root.remove("profiles");
    let legacy_profiles = root.remove("profile");
    match (profiles, legacy_profiles) {
        (Some(_), Some(_)) => Err(ConfigError::CompositionError(
            "use either [profiles.*] or legacy [profile.*], not both".to_string(),
        )),
        (Some(value), None) | (None, Some(value)) => named_table(value, "profiles"),
        (None, None) => Ok(Table::new()),
    }
}

fn take_named_tables(root: &mut Table, key: &str) -> Result<Table, ConfigError> {
    match root.remove(key) {
        Some(value) => named_table(value, key),
        None => Ok(Table::new()),
    }
}

fn named_table(value: Value, kind: &str) -> Result<Table, ConfigError> {
    match value {
        Value::Table(table) => Ok(table),
        _ => Err(ConfigError::CompositionError(format!(
            "'{kind}' must be a TOML table"
        ))),
    }
}

fn take_bundles(
    root: &mut Table,
) -> Result<std::collections::BTreeMap<String, BundleDefinition>, ConfigError> {
    let tables = take_named_tables(root, "bundles")?;
    tables
        .into_iter()
        .map(|(name, value)| {
            let definition = value.try_into().map_err(|error| {
                ConfigError::CompositionError(format!("invalid bundle '{name}': {error}"))
            })?;
            Ok((name, definition))
        })
        .collect()
}

fn combine_names(first: &[String], second: &[String]) -> Vec<String> {
    first.iter().chain(second).cloned().collect()
}

fn reject_duplicates(kind: &str, names: &[String]) -> Result<(), ConfigError> {
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(ConfigError::CompositionError(format!(
                "duplicate {kind} '{name}' in composition"
            )));
        }
    }
    Ok(())
}

fn reject_conflicting_profiles(profiles: &[String]) -> Result<(), ConfigError> {
    if profiles.iter().any(|profile| profile == "dev")
        && profiles.iter().any(|profile| profile == "prod")
    {
        return Err(ConfigError::CompositionError(
            "profiles 'dev' and 'prod' cannot be combined".to_string(),
        ));
    }
    Ok(())
}

fn apply_named_overlay(
    document: &mut Value,
    overlays: &Table,
    kind: &str,
    name: &str,
) -> Result<(), ConfigError> {
    let value = overlays
        .get(name)
        .ok_or_else(|| ConfigError::CompositionError(format!("unknown {kind} '{name}'")))?;
    validate_overlay_fields(value, kind, name)?;
    overlay::merge(document, value.clone())
        .map_err(|error| ConfigError::CompositionError(format!("invalid {kind} '{name}': {error}")))
}

fn validate_overlay_fields(value: &Value, kind: &str, name: &str) -> Result<(), ConfigError> {
    let root = value.as_table().ok_or_else(|| {
        ConfigError::CompositionError(format!("{kind} '{name}' must be a TOML table"))
    })?;
    for (section, settings) in root {
        let allowed: &[&str] = match section.as_str() {
            "server" => &[
                "bind_addr",
                "port",
                "api_key",
                "cors_enabled",
                "cors_origins",
                "concurrency_limit",
                "body_limit_bytes",
                "rate_limit",
                "metrics_enabled",
            ],
            "storage" => &["data_dir", "media_ttl_days"],
            "transport" => &["tls_profile", "proxy_url", "timeout_secs"],
            "video" => &["enabled"],
            _ => return Err(unknown_overlay_field(kind, name, section)),
        };
        let Some(settings) = settings.as_table() else {
            return Err(ConfigError::CompositionError(format!(
                "invalid {kind} '{name}': section '{section}' must be a TOML table"
            )));
        };
        for (setting, value) in settings {
            if !allowed.contains(&setting.as_str()) {
                return Err(unknown_overlay_field(
                    kind,
                    name,
                    &format!("{section}.{setting}"),
                ));
            }
            if section == "server" && setting == "rate_limit" {
                let Some(rate_limit) = value.as_table() else {
                    return Err(ConfigError::CompositionError(format!(
                        "invalid {kind} '{name}': section 'server.rate_limit' must be a TOML table"
                    )));
                };
                for nested in rate_limit.keys() {
                    if !["capacity", "refill_tokens", "refill_interval_secs"]
                        .contains(&nested.as_str())
                    {
                        return Err(unknown_overlay_field(
                            kind,
                            name,
                            &format!("server.rate_limit.{nested}"),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn unknown_overlay_field(kind: &str, name: &str, field: &str) -> ConfigError {
    ConfigError::CompositionError(format!(
        "invalid {kind} '{name}': unknown configuration field '{field}'"
    ))
}

/// Build a Figment seeded with all hardcoded defaults.
fn defaults_figment() -> Figment {
    // Construct a default BridgeConfig and serialize it to a toml string so
    // figment can use it as the lowest-priority provider.
    let default_server = ServerConfig::default();
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
    let server = &config.server;
    if server.port == 0 {
        return Err(ConfigError::ValidationError(
            "server.port must not be 0".to_string(),
        ));
    }
    if !["chrome", "firefox", "safari"]
        .contains(&config.transport.tls_profile.to_ascii_lowercase().as_str())
    {
        return Err(ConfigError::ValidationError(format!(
            "transport.tls_profile '{}' is not a supported preset; expected one of chrome, firefox, safari",
            config.transport.tls_profile
        )));
    }
    if let Some(api_key) = &server.api_key
        && (api_key.trim().is_empty() || api_key.chars().count() < 32)
    {
        return Err(ConfigError::ValidationError(
            "server.api_key must be non-empty and at least 32 characters".to_string(),
        ));
    }
    if server.cors_enabled && server.cors_origins.is_empty() {
        return Err(ConfigError::ValidationError(
            "server.cors_enabled requires at least one explicit server.cors_origins entry"
                .to_string(),
        ));
    }
    for origin in &server.cors_origins {
        validate_cors_origin(origin)?;
    }
    if server.concurrency_limit == 0 {
        return Err(ConfigError::ValidationError(
            "server.concurrency_limit must be greater than 0".to_string(),
        ));
    }
    if server.body_limit_bytes == 0 {
        return Err(ConfigError::ValidationError(
            "server.body_limit_bytes must be greater than 0".to_string(),
        ));
    }
    if let Some(rate_limit) = &server.rate_limit
        && (rate_limit.capacity == 0
            || rate_limit.refill_tokens == 0
            || rate_limit.refill_interval_secs == 0)
    {
        return Err(ConfigError::ValidationError(
            "server.rate_limit values must be greater than 0".to_string(),
        ));
    }
    if config.video.enabled {
        return Err(ConfigError::ValidationError(
            "video.enabled=true is unsupported because this binary has no production video adapter"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_cors_origin(origin: &str) -> Result<(), ConfigError> {
    let valid_scheme = origin.starts_with("https://") || origin.starts_with("http://");
    let authority = origin
        .split_once("://")
        .map(|(_, authority)| authority)
        .unwrap_or_default();
    if origin == "*"
        || !valid_scheme
        || authority.is_empty()
        || authority.contains('*')
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
        || origin.chars().any(char::is_whitespace)
    {
        return Err(ConfigError::ValidationError(format!(
            "server.cors_origins entry '{origin}' must be an explicit HTTP(S) origin without path, query, fragment, whitespace, or wildcard"
        )));
    }
    Ok(())
}
