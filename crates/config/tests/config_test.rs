use std::fs;

use gemini_bridge_config::{BridgeConfig, ConfigError};
use tempfile::tempdir;

#[test]
fn empty_toml_applies_all_defaults() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(&path, "").unwrap();

    let config = BridgeConfig::load_from_file(&path).unwrap();
    assert_eq!(config.server.bind_addr, "127.0.0.1");
    assert_eq!(config.server.port, 8090);
    assert_eq!(config.server.api_key, None);
    assert!(!config.server.cors_enabled);
    assert_eq!(config.storage.data_dir, default_data_dir());
    assert_eq!(config.storage.media_ttl_days, 30);
    assert_eq!(config.transport.tls_profile, "chrome");
    assert_eq!(config.transport.proxy_url, None);
    assert_eq!(config.transport.timeout_secs, 30);
}

#[test]
fn bridge_server_port_environment_override_changes_port() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(&path, "").unwrap();

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("environment_override_child")
        .arg("--nocapture")
        .env("CONFIG_TEST_PATH", &path)
        .env("BRIDGE_SERVER_PORT", "9000")
        .status()
        .unwrap();

    assert!(status.success());
}

#[test]
fn environment_override_child() {
    let Some(path) = std::env::var_os("CONFIG_TEST_PATH") else {
        return;
    };
    let config = BridgeConfig::load_from_file(std::path::Path::new(&path)).unwrap();
    assert_eq!(config.server.port, 9000);
}
#[test]
fn port_zero_is_rejected() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(&path, "[server]\nport = 0\n").unwrap();

    assert!(matches!(
        BridgeConfig::load_from_file(&path),
        Err(ConfigError::ValidationError(_))
    ));
}

#[test]
fn api_key_is_redacted_from_debug_output() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(&path, "[server]\napi_key = 'top-secret-value'\n").unwrap();

    let config = BridgeConfig::load_from_file(&path).unwrap();
    let debug = format!("{:?}", config);
    assert!(!debug.contains("top-secret-value"));
    assert!(debug.contains("***REDACTED***"));
}

fn default_data_dir() -> std::path::PathBuf {
    let mut path = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                let mut path = std::path::PathBuf::from(home);
                path.push(".local");
                path.push("share");
                path
            })
        })
        .unwrap_or_else(|| std::path::PathBuf::from("~/.local/share"));
    path.push("gemini-bridge");
    path
}
