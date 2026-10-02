use std::fs;

use gemini_bridge_config::{BridgeConfig, Composition, ConfigError};
use tempfile::tempdir;

#[test]
fn profiles_can_enable_metrics() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(
        &path,
        r#"
[profiles.observability.server]
metrics_enabled = true
"#,
    )
    .unwrap();

    let config = BridgeConfig::load_composed(
        Some(&path),
        &Composition::new().with_profile("observability"),
    )
    .unwrap();

    assert!(config.server.metrics_enabled);
}

#[test]
fn composes_profiles_bundles_and_patches_in_deterministic_order() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(
        &path,
        r#"
[server]
port = 8000
cors_enabled = false

[storage]
media_ttl_days = 30

[transport]
timeout_secs = 30

[profiles.dev.server]
port = 8100
bind_addr = "bundle-first"

[profiles.lowmem.storage]
media_ttl_days = 7

[profiles.warm.server]
port = 8150

[profiles.tuned.transport]
timeout_secs = 25

[profiles.tuned-later.server]
bind_addr = "profile-last"

[patches.network.transport]
timeout_secs = 20

[patches.network.video]
enabled = false

[patches.network-later.transport]
timeout_secs = 15

[patches.network-later.video]
enabled = true

[patches.operator.server]
port = 8300
cors_enabled = true

[patches.operator.transport]
timeout_secs = 5

[bundles.edge]
profiles = ["dev", "lowmem"]
patches = ["network"]

[bundles.edge-later]
profiles = ["warm"]
patches = ["network-later"]
"#,
    )
    .unwrap();

    let composition = Composition::new()
        .with_bundle("edge")
        .with_bundle("edge-later")
        .with_profile("tuned")
        .with_profile("tuned-later")
        .with_patch("operator");
    let config = BridgeConfig::load_composed(Some(&path), &composition).unwrap();

    assert_eq!(config.server.port, 8300);
    assert!(config.server.cors_enabled);
    assert_eq!(config.server.bind_addr, "profile-last");
    assert_eq!(config.storage.media_ttl_days, 7);
    assert_eq!(config.transport.timeout_secs, 5);
    assert!(config.video.enabled);
}

#[test]
fn environment_values_override_the_full_composition() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(
        &path,
        r#"
[profiles.dev.server]
port = 8100

[bundles.local]
profiles = ["dev"]
"#,
    )
    .unwrap();

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("environment_composition_child")
        .arg("--nocapture")
        .env("CONFIG_COMPOSITION_TEST_PATH", &path)
        .env("BRIDGE_SERVER_PORT", "9000")
        .status()
        .unwrap();

    assert!(status.success());
}

#[test]
fn environment_composition_child() {
    let Some(path) = std::env::var_os("CONFIG_COMPOSITION_TEST_PATH") else {
        return;
    };
    let config = BridgeConfig::load_composed(
        Some(std::path::Path::new(&path)),
        &Composition::new().with_bundle("local"),
    )
    .unwrap();
    assert_eq!(config.server.port, 9000);
}

#[test]
fn rejects_dev_and_prod_profiles_as_mutually_exclusive() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(
        &path,
        r#"
[profiles.dev.server]
port = 8100

[profiles.prod.server]
port = 8200
"#,
    )
    .unwrap();

    let error = BridgeConfig::load_composed(
        Some(&path),
        &Composition::new().with_profile("dev").with_profile("prod"),
    )
    .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("dev"), "{message}");
    assert!(message.contains("prod"), "{message}");
    assert!(message.contains("cannot be combined"), "{message}");
}

#[test]
fn rejects_unknown_profile_configuration_field() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(
        &path,
        r#"
[profiles.dev.server]
unknown_setting = true
"#,
    )
    .unwrap();

    let error = BridgeConfig::load_composed(Some(&path), &Composition::new().with_profile("dev"))
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("dev"), "{message}");
    assert!(message.contains("server.unknown_setting"), "{message}");
}

#[test]
fn rejects_misspelled_bundle_properties_with_context() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(
        &path,
        r#"
[bundles.edge]
profile = ["dev"]
"#,
    )
    .unwrap();

    let error = BridgeConfig::load_composed(Some(&path), &Composition::new().with_bundle("edge"))
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("edge"), "{message}");
    assert!(message.contains("profile"), "{message}");
}

#[test]
fn rejects_unknown_composition_member_with_its_kind_and_name() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(&path, "").unwrap();

    let error = BridgeConfig::load_composed(
        Some(&path),
        &Composition::new().with_bundle("missing-bundle"),
    )
    .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("bundle"), "{message}");
    assert!(message.contains("missing-bundle"), "{message}");
}

#[test]
fn rejects_bundle_referencing_an_unknown_profile() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(
        &path,
        r#"
[bundles.edge]
profiles = ["missing-profile"]
"#,
    )
    .unwrap();

    let error = BridgeConfig::load_composed(Some(&path), &Composition::new().with_bundle("edge"))
        .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("profile"), "{message}");
    assert!(message.contains("missing-profile"), "{message}");
    assert!(message.contains("edge"), "{message}");
}

#[test]
fn rejects_duplicate_profile_selection() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(
        &path,
        r#"
[profiles.dev.server]
port = 8100
"#,
    )
    .unwrap();

    let error = BridgeConfig::load_composed(
        Some(&path),
        &Composition::new().with_profile("dev").with_profile("dev"),
    )
    .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("duplicate profile"), "{message}");
    assert!(message.contains("dev"), "{message}");
}

#[test]
fn legacy_profile_loader_accepts_legacy_toml_profile_section() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(
        &path,
        r#"
[server]
port = 8000

[profile.dev.server]
port = 8100
"#,
    )
    .unwrap();

    let config = BridgeConfig::load_with_overrides(Some(&path), Some("dev")).unwrap();
    assert_eq!(config.server.port, 8100);
}

#[test]
fn legacy_loader_without_a_file_ignores_profile_and_uses_defaults() {
    let config = BridgeConfig::load_with_overrides(None, Some("missing-profile")).unwrap();
    assert_eq!(config.server.port, 8090);
}

#[test]
fn malformed_toml_keeps_the_parse_error_variant() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(&path, "[server\nport = 9000").unwrap();

    assert!(matches!(
        BridgeConfig::load_from_file(&path),
        Err(ConfigError::ParseError(_))
    ));
}

#[test]
fn legacy_base_configuration_still_ignores_unknown_fields() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    fs::write(&path, "[server]\nport = 9000\nlegacy_extension = true\n").unwrap();

    let config = BridgeConfig::load_from_file(&path).unwrap();
    assert_eq!(config.server.port, 9000);
}
