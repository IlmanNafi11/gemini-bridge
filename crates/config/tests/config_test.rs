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
    assert!(!config.server.metrics_enabled);
    assert!(config.server.cors_origins.is_empty());
    assert_eq!(config.server.concurrency_limit, 4);
    assert_eq!(config.server.body_limit_bytes, 10 * 1024 * 1024);
    let rate_limit = config.server.rate_limit.unwrap();
    assert_eq!(rate_limit.capacity, 60);
    assert_eq!(rate_limit.refill_tokens, 60);
    assert_eq!(rate_limit.refill_interval_secs, 60);
    assert_eq!(config.storage.data_dir, default_data_dir());
    assert_eq!(config.storage.media_ttl_days, 30);
    assert_eq!(config.transport.tls_profile, "chrome");
    assert_eq!(config.transport.proxy_url, None);
    assert_eq!(config.transport.timeout_secs, 30);
    assert!(!config.video.enabled);
}

#[test]
fn server_rate_limit_can_be_disabled_explicitly() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(&path, "[server]\nrate_limit = false\n").unwrap();

    let config = BridgeConfig::load_from_file(&path).unwrap();
    assert_eq!(config.server.rate_limit, None);
}

#[test]
fn metrics_can_be_enabled_explicitly() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(&path, "[server]\nmetrics_enabled = true\n").unwrap();

    let config = BridgeConfig::load_from_file(&path).unwrap();
    assert!(config.server.metrics_enabled);
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
    fs::write(
        &path,
        "[server]\napi_key = 'top-secret-value-012345678901234567890123'\n",
    )
    .unwrap();

    let config = BridgeConfig::load_from_file(&path).unwrap();
    let debug = format!("{:?}", config);
    assert!(!debug.contains("top-secret-value-012345678901234567890123"));
    assert!(debug.contains("***REDACTED***"));
}

#[test]
fn rejects_empty_and_weak_api_keys() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");

    for api_key in ["", "short", "                                "] {
        fs::write(&path, format!("[server]\napi_key = '{api_key}'\n")).unwrap();
        assert!(matches!(
            BridgeConfig::load_from_file(&path),
            Err(ConfigError::ValidationError(_))
        ));
    }
}

#[test]
fn accepts_strong_api_key_and_explicit_cors_origins() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(
        &path,
        "[server]\napi_key = '0123456789abcdef0123456789abcdef'\ncors_enabled = true\ncors_origins = ['https://client.example']\n",
    )
    .unwrap();

    let config = BridgeConfig::load_from_file(&path).unwrap();
    assert_eq!(config.server.cors_origins, vec!["https://client.example"]);
}

#[test]
fn rejects_wildcard_or_unscoped_cors_origins() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    for origin in [
        "*",
        "https://*.example.com",
        "https://client.example/path",
        "client.example",
    ] {
        fs::write(
            &path,
            format!("[server]\ncors_enabled = true\ncors_origins = ['{origin}']\n"),
        )
        .unwrap();
        assert!(matches!(
            BridgeConfig::load_from_file(&path),
            Err(ConfigError::ValidationError(_))
        ));
    }
}

#[test]
fn parses_production_request_limits() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(
        &path,
        "[server]\nconcurrency_limit = 8\nbody_limit_bytes = 1048576\n[server.rate_limit]\ncapacity = 10\nrefill_tokens = 5\nrefill_interval_secs = 30\n",
    )
    .unwrap();

    let config = BridgeConfig::load_from_file(&path).unwrap();
    assert_eq!(config.server.concurrency_limit, 8);
    assert_eq!(config.server.body_limit_bytes, 1_048_576);
    let rate_limit = config.server.rate_limit.unwrap();
    assert_eq!(rate_limit.capacity, 10);
    assert_eq!(rate_limit.refill_tokens, 5);
    assert_eq!(rate_limit.refill_interval_secs, 30);
}

#[test]
fn rejects_unsupported_tls_profiles_instead_of_falling_back() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    for profile in ["ja3", "ja4", "unknown"] {
        fs::write(&path, format!("[transport]\ntls_profile = '{profile}'\n")).unwrap();
        let error = BridgeConfig::load_from_file(&path).unwrap_err();
        assert!(matches!(error, ConfigError::ValidationError(_)));
        assert!(error.to_string().contains("tls_profile"));
    }
    for profile in ["chrome", "FIREFOX", "Safari"] {
        fs::write(&path, format!("[transport]\ntls_profile = '{profile}'\n")).unwrap();
        assert!(BridgeConfig::load_from_file(&path).is_ok());
    }
}

#[test]
fn rejects_enabled_video_without_production_adapter() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(&path, "[video]\nenabled = true\n").unwrap();

    let error = BridgeConfig::load_from_file(&path).unwrap_err();
    assert!(matches!(error, ConfigError::ValidationError(_)));
    assert!(error.to_string().contains("no production video adapter"));
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
