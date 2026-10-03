use std::sync::LazyLock;
use tokio::sync::Mutex;

use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, KeyInit},
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

use gemini_bridge_identity::crypto::{decrypt, encrypt, validate_bridge_secret};
use gemini_bridge_identity::parser::{
    compute_sapisidhash, extract_bl, extract_fsid, extract_snlm0e,
};
use gemini_bridge_identity::storage::{read_cookies, write_cookies};
use gemini_bridge_identity::{SessionBootstrap, SessionCredentials};

// ─── Fixtures ────────────────────────────────────────────────────────────────

const SAMPLE_GEMINI_HTML: &str = r#"
<!doctype html>
<html>
<head>
<script>
window.WIZ_global_data = {
  "cfb2h": "boq_assistant-bard-web-server_20240101.00_p0",
  "SNlM0e": "AIzaSyFakeTokenForTesting1234567890",
  "FdrFJe": "1234567890123456789"
};
</script>
</head>
<body>
</body>
</html>
"#;

const SAMPLE_GEMINI_HTML_ARRAY_VARIANT: &str = r#"
<!doctype html>
<html>
<head>
<script>
window.WIZ_global_data = {
  "cfb2h": "boq_assistant-bard-web-server_20240202.01_p1"
};
window._data = [["SNlM0e","ArrayFormatToken9876543210"], ["FdrFJe","9876543210987654321"]];
</script>
</head>
</html>
"#;

// ─── 1. Parser Tests ──────────────────────────────────────────────────────────

#[test]
fn test_parser_extracts_bl_from_cfb2h() {
    let bl = extract_bl(SAMPLE_GEMINI_HTML);
    assert_eq!(
        bl.as_deref(),
        Some("boq_assistant-bard-web-server_20240101.00_p0")
    );
}

#[test]
fn test_parser_extracts_bl_from_regex_fallback() {
    let html = r#"<script>var x = "boq_assistant-bard-web-server_20240303.00_p0";</script>"#;
    let bl = extract_bl(html);
    assert_eq!(
        bl.as_deref(),
        Some("boq_assistant-bard-web-server_20240303.00_p0")
    );
}

#[test]
fn test_parser_extracts_snlm0e() {
    let token = extract_snlm0e(SAMPLE_GEMINI_HTML);
    assert_eq!(
        token.as_deref(),
        Some("AIzaSyFakeTokenForTesting1234567890")
    );
}

#[test]
fn test_parser_extracts_snlm0e_array_variant() {
    let token = extract_snlm0e(SAMPLE_GEMINI_HTML_ARRAY_VARIANT);
    assert_eq!(token.as_deref(), Some("ArrayFormatToken9876543210"));
}

#[test]
fn test_parser_extracts_fsid() {
    let fsid = extract_fsid(SAMPLE_GEMINI_HTML);
    assert_eq!(fsid.as_deref(), Some("1234567890123456789"));
}

#[test]
fn test_parser_builds_sapisidhash_deterministic() {
    // Fixed timestamp = 1700000000, known sapisid
    let sapisid = "test_sapisid_value";
    let hash = compute_sapisidhash(1700000000, sapisid);
    assert!(hash.starts_with("SAPISIDHASH 1700000000_"));
    // Verify SHA-1 is 40 hex characters
    let hex_part = &hash["SAPISIDHASH 1700000000_".len()..];
    assert_eq!(hex_part.len(), 40);
    // Hash is deterministic
    let hash2 = compute_sapisidhash(1700000000, sapisid);
    assert_eq!(hash, hash2);
}

#[test]
fn test_parser_missing_fields_return_none() {
    let empty_html = "<html><body>Empty</body></html>";
    assert!(extract_bl(empty_html).is_none());
    assert!(extract_snlm0e(empty_html).is_none());
    assert!(extract_fsid(empty_html).is_none());
}

// ─── 2. Crypto Tests ─────────────────────────────────────────────────────────

// Serial lock for environment variable modifications. Tests that construct an
// identity service also take this lock because startup validates BRIDGE_SECRET.
static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
const STRONG_SECRET_A: &str = "correct-horse-battery-staple-bridge-A";
const STRONG_SECRET_B: &str = "correct-horse-battery-staple-bridge-B";

#[test]
fn test_crypto_round_trip_uses_versioned_salted_kdf_envelope() {
    let _guard = ENV_LOCK.blocking_lock();
    unsafe { std::env::set_var("BRIDGE_SECRET", STRONG_SECRET_A) };

    let plaintext = b"{\"psid\":\"foo\",\"psidts\":\"bar\",\"sapisid\":\"baz\"}";
    let first = encrypt(plaintext).unwrap().expect("encryption enabled");
    let second = encrypt(plaintext).unwrap().expect("encryption enabled");

    assert!(
        first.starts_with(b"GBCK"),
        "new ciphertext must be versioned"
    );
    assert_ne!(
        first, second,
        "each encryption must use a fresh salt and nonce"
    );
    assert_eq!(decrypt(&first).unwrap(), plaintext);
    assert_eq!(decrypt(&second).unwrap(), plaintext);

    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[test]
fn test_crypto_fails_with_wrong_key_without_disclosing_secrets() {
    let _guard = ENV_LOCK.blocking_lock();
    unsafe { std::env::set_var("BRIDGE_SECRET", STRONG_SECRET_A) };
    let ciphertext = encrypt(b"sensitive-data").unwrap().unwrap();

    unsafe { std::env::set_var("BRIDGE_SECRET", STRONG_SECRET_B) };
    let error = decrypt(&ciphertext).unwrap_err();
    let rendered = format!("{error:?}: {error}");
    assert!(!rendered.contains(STRONG_SECRET_A));
    assert!(!rendered.contains(STRONG_SECRET_B));
    assert!(!rendered.contains("sensitive-data"));

    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[test]
fn test_crypto_rejects_empty_short_and_low_diversity_secrets_actionably() {
    let _guard = ENV_LOCK.blocking_lock();
    for weak in ["", "too-short", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"] {
        unsafe { std::env::set_var("BRIDGE_SECRET", weak) };
        let error = validate_bridge_secret().unwrap_err();
        let message = error.to_string();
        assert!(message.contains("BRIDGE_SECRET"));
        assert!(message.contains("32 bytes"));
        assert!(!message.contains(weak) || weak.is_empty());
    }

    unsafe { std::env::remove_var("BRIDGE_SECRET") };
    assert!(
        !validate_bridge_secret().unwrap(),
        "unset secret keeps optional plaintext mode"
    );
}
#[test]
fn identity_service_rejects_weak_bridge_secret_without_cookie_file() {
    use gemini_bridge_config::{BridgeConfig, ServerConfig, StorageConfig, TransportConfig};
    use gemini_bridge_identity::{DefaultIdentityService, IdentityError};

    let _guard = ENV_LOCK.blocking_lock();
    unsafe { std::env::set_var("BRIDGE_SECRET", "weak") };
    let dir = tempdir().unwrap();
    let config = BridgeConfig {
        server: ServerConfig::default(),
        storage: StorageConfig {
            data_dir: dir.path().to_path_buf(),
            media_ttl_days: 30,
        },
        transport: TransportConfig {
            tls_profile: "chrome".into(),
            proxy_url: None,
            timeout_secs: 5,
        },
        video: Default::default(),
    };

    let error = DefaultIdentityService::new(&config).err().unwrap();
    assert!(matches!(error, IdentityError::WeakBridgeSecret));
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

// ─── 3. Storage Tests ─────────────────────────────────────────────────────────

#[test]
fn test_storage_plaintext_round_trip_when_secret_is_unset() {
    let _guard = ENV_LOCK.blocking_lock();
    unsafe { std::env::remove_var("BRIDGE_SECRET") };

    let dir = tempdir().unwrap();
    let json = r#"{"psid":"test_psid","psidts":"test_psidts","sapisid":"test_sapisid","imported_at":"2024-01-01T00:00:00Z"}"#;
    write_cookies(dir.path(), json).expect("write cookies");
    assert_eq!(read_cookies(dir.path()).unwrap().as_deref(), Some(json));
}

#[test]
fn test_storage_encrypted_round_trip() {
    let _guard = ENV_LOCK.blocking_lock();
    unsafe { std::env::set_var("BRIDGE_SECRET", STRONG_SECRET_A) };

    let dir = tempdir().unwrap();
    let json = r#"{"psid":"secure_psid","psidts":"secure_psidts","sapisid":"secure_sapisid","imported_at":"2024-01-01T00:00:00Z"}"#;
    write_cookies(dir.path(), json).expect("write encrypted cookies");

    let raw_bytes = std::fs::read(dir.path().join("cookies.json")).unwrap();
    assert!(raw_bytes.starts_with(b"GBCK"));
    assert_ne!(raw_bytes, json.as_bytes());
    assert_eq!(read_cookies(dir.path()).unwrap().as_deref(), Some(json));

    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[test]
fn test_storage_encrypted_fails_with_wrong_key() {
    let _guard = ENV_LOCK.blocking_lock();
    unsafe { std::env::set_var("BRIDGE_SECRET", STRONG_SECRET_A) };

    let dir = tempdir().unwrap();
    write_cookies(dir.path(), r#"{"psid":"a"}"#).unwrap();
    unsafe { std::env::set_var("BRIDGE_SECRET", STRONG_SECRET_B) };
    assert!(
        read_cookies(dir.path()).is_err(),
        "wrong key must fail closed"
    );

    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[test]
fn test_storage_reads_and_migrates_legacy_sha256_ciphertext() {
    let _guard = ENV_LOCK.blocking_lock();
    unsafe { std::env::set_var("BRIDGE_SECRET", STRONG_SECRET_A) };

    let dir = tempdir().unwrap();
    let json = r#"{"psid":"legacy","psidts":"old","sapisid":"cookie","imported_at":"2024-01-01T00:00:00Z"}"#;
    let key_bytes = Sha256::digest(STRONG_SECRET_A.as_bytes());
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));
    let nonce_bytes = [7_u8; 12];
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), json.as_bytes())
        .unwrap();
    let mut legacy = nonce_bytes.to_vec();
    legacy.extend_from_slice(&ciphertext);
    std::fs::write(dir.path().join("cookies.json"), &legacy).unwrap();

    assert_eq!(read_cookies(dir.path()).unwrap().as_deref(), Some(json));
    let migrated = std::fs::read(dir.path().join("cookies.json")).unwrap();
    assert!(migrated.starts_with(b"GBCK"));
    assert_ne!(migrated, legacy);

    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}
#[cfg(unix)]
#[test]
fn test_legacy_rewrap_failure_preserves_ciphertext_and_fails_closed() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = ENV_LOCK.blocking_lock();
    unsafe { std::env::set_var("BRIDGE_SECRET", STRONG_SECRET_A) };

    let dir = tempdir().unwrap();
    let json = r#"{"psid":"legacy secret","psidts":"old","sapisid":"cookie","imported_at":"2024-01-01T00:00:00Z"}"#;
    let key_bytes = Sha256::digest(STRONG_SECRET_A.as_bytes());
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));
    let nonce_bytes = [7_u8; 12];
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), json.as_bytes())
        .unwrap();
    let mut legacy = nonce_bytes.to_vec();
    legacy.extend_from_slice(&encrypted);
    let path = dir.path().join("cookies.json");
    std::fs::write(&path, &legacy).unwrap();

    // Root can bypass mode bits, so skip only when a write probe confirms this
    // environment does not enforce a read-only directory.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    if std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.path().join("permission-probe"))
        .is_ok()
    {
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        unsafe { std::env::remove_var("BRIDGE_SECRET") };
        return;
    }

    let error = read_cookies(dir.path()).expect_err("failed legacy rewrap must fail closed");
    assert!(matches!(
        &error,
        gemini_bridge_identity::IdentityError::LegacyCredentialRewrap
    ));
    let rendered = format!("{error:?}: {error}");
    assert!(!rendered.contains(STRONG_SECRET_A));
    assert!(!rendered.contains("legacy secret"));
    assert_eq!(std::fs::read(path).unwrap(), legacy);

    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[test]
fn test_failed_write_preserves_existing_file() {
    let _guard = ENV_LOCK.blocking_lock();
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
    let dir = tempdir().unwrap();
    write_cookies(dir.path(), "old credentials").unwrap();
    let before = std::fs::read(dir.path().join("cookies.json")).unwrap();

    unsafe { std::env::set_var("BRIDGE_SECRET", "weak") };
    assert!(write_cookies(dir.path(), "replacement credentials").is_err());
    assert_eq!(
        std::fs::read(dir.path().join("cookies.json")).unwrap(),
        before
    );
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[cfg(unix)]
#[test]
fn test_storage_atomically_replaces_file_with_0600_mode() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let _guard = ENV_LOCK.blocking_lock();
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
    let dir = tempdir().unwrap();
    write_cookies(dir.path(), "first").unwrap();
    let path = dir.path().join("cookies.json");
    let first_inode = std::fs::metadata(&path).unwrap().ino();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    write_cookies(dir.path(), "second").unwrap();
    let metadata = std::fs::metadata(&path).unwrap();
    assert_ne!(
        metadata.ino(),
        first_inode,
        "replacement must use rename, not in-place truncation"
    );
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "second");
}

// ─── 4. Redaction Tests ───────────────────────────────────────────────────────

#[test]
fn test_credentials_debug_is_redacted() {
    let creds = SessionCredentials {
        psid: "super_secret_psid".to_string(),
        psidts: "super_secret_psidts".to_string(),
        sapisid: "super_secret_sapisid".to_string(),
        imported_at: "2024-01-01T00:00:00Z".to_string(),
    };
    let debug_str = format!("{:?}", creds);
    assert!(!debug_str.contains("super_secret_psid"));
    assert!(!debug_str.contains("super_secret_psidts"));
    assert!(!debug_str.contains("super_secret_sapisid"));
    assert!(debug_str.contains("<redacted>"));
}

#[test]
fn test_bootstrap_debug_is_redacted() {
    let bootstrap = SessionBootstrap {
        bl: "boq_secret_bl".to_string(),
        snlm0e: "secret_snlm0e_token".to_string(),
        fsid: "secret_fsid".to_string(),
    };
    let debug_str = format!("{:?}", bootstrap);
    assert!(!debug_str.contains("boq_secret_bl"));
    assert!(!debug_str.contains("secret_snlm0e_token"));
    assert!(!debug_str.contains("secret_fsid"));
    assert!(debug_str.contains("<redacted>"));
}
#[tokio::test]
async fn failed_candidate_validation_preserves_disk_and_live_credentials() {
    use gemini_bridge_config::{BridgeConfig, ServerConfig, StorageConfig, TransportConfig};
    use gemini_bridge_identity::{DefaultIdentityService, IdentityService};
    use http::HeaderMap;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _guard = ENV_LOCK.lock().await;
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
    let server = MockServer::start().await;
    let dir = tempdir().unwrap();
    let config = BridgeConfig {
        server: ServerConfig {
            bind_addr: "127.0.0.1".into(),
            port: 8090,
            api_key: None,
            cors_enabled: false,
            cors_origins: Vec::new(),
            concurrency_limit: 4,
            body_limit_bytes: 10 * 1024 * 1024,
            rate_limit: None,
            metrics_enabled: false,
        },
        storage: StorageConfig {
            data_dir: dir.path().to_path_buf(),
            media_ttl_days: 30,
        },
        transport: TransportConfig {
            tls_profile: "chrome".into(),
            proxy_url: None,
            timeout_secs: 5,
        },
        video: Default::default(),
    };
    let service = DefaultIdentityService::with_base_url(&config, Some(server.uri())).unwrap();
    service
        .import_credentials("__Secure-1PSID=old_psid; __Secure-1PSIDTS=old_ts; SAPISID=old_sapisid")
        .await
        .unwrap();
    let disk_before = std::fs::read(dir.path().join("cookies.json")).unwrap();

    Mock::given(method("GET"))
        .and(path("/app"))
        .and(header(
            "cookie",
            "__Secure-1PSID=bad_psid; __Secure-1PSIDTS=bad_ts; SAPISID=bad_sapisid",
        ))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    let error = service
        .import_credentials_validated(
            "__Secure-1PSID=bad_psid; __Secure-1PSIDTS=bad_ts; SAPISID=bad_sapisid",
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        gemini_bridge_identity::IdentityError::NeedsReauth
    ));
    assert_eq!(
        std::fs::read(dir.path().join("cookies.json")).unwrap(),
        disk_before
    );

    let mut headers = HeaderMap::new();
    service.apply_auth_headers(&mut headers).unwrap();
    let cookie = headers.get(http::header::COOKIE).unwrap().to_str().unwrap();
    assert!(cookie.contains("old_psid"));
    assert!(!cookie.contains("bad_psid"));
}

#[tokio::test]
async fn validated_candidate_replaces_disk_and_live_credentials_after_probe() {
    use gemini_bridge_config::{BridgeConfig, ServerConfig, StorageConfig, TransportConfig};
    use gemini_bridge_identity::{DefaultIdentityService, IdentityService, SessionStatus};
    use http::HeaderMap;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _guard = ENV_LOCK.lock().await;
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .and(header(
            "cookie",
            "__Secure-1PSID=new_psid; __Secure-1PSIDTS=new_ts; SAPISID=new_sapisid",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_GEMINI_HTML))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempdir().unwrap();
    let config = BridgeConfig {
        server: ServerConfig {
            bind_addr: "127.0.0.1".into(),
            port: 8090,
            api_key: None,
            cors_enabled: false,
            cors_origins: Vec::new(),
            concurrency_limit: 4,
            body_limit_bytes: 10 * 1024 * 1024,
            rate_limit: None,
            metrics_enabled: false,
        },
        storage: StorageConfig {
            data_dir: dir.path().to_path_buf(),
            media_ttl_days: 30,
        },
        transport: TransportConfig {
            tls_profile: "chrome".into(),
            proxy_url: None,
            timeout_secs: 5,
        },
        video: Default::default(),
    };
    let service = DefaultIdentityService::with_base_url(&config, Some(server.uri())).unwrap();
    service
        .import_credentials("__Secure-1PSID=old_psid; __Secure-1PSIDTS=old_ts; SAPISID=old_sapisid")
        .await
        .unwrap();

    let bootstrap = service
        .import_credentials_validated(
            "__Secure-1PSID=new_psid; __Secure-1PSIDTS=new_ts; SAPISID=new_sapisid",
        )
        .await
        .unwrap();
    assert_eq!(bootstrap.bl, "boq_assistant-bard-web-server_20240101.00_p0");
    assert_eq!(service.snapshot().await.status, SessionStatus::Valid);
    let persisted = read_cookies(dir.path()).unwrap().unwrap();
    assert!(persisted.contains("new_psid"));
    assert!(!persisted.contains("old_psid"));

    let mut headers = HeaderMap::new();
    service.apply_auth_headers(&mut headers).unwrap();
    let cookie = headers.get(http::header::COOKIE).unwrap().to_str().unwrap();
    assert!(cookie.contains("new_psid"));
    assert!(!cookie.contains("old_psid"));
}

#[tokio::test]
async fn bootstrap_uses_imported_credentials_and_extracts_tokens() {
    use gemini_bridge_config::{BridgeConfig, ServerConfig, StorageConfig, TransportConfig};
    use gemini_bridge_identity::{DefaultIdentityService, IdentityService, SessionStatus};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let _guard = ENV_LOCK.lock().await;
    unsafe { std::env::remove_var("BRIDGE_SECRET") };

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_GEMINI_HTML))
        .mount(&server)
        .await;

    let dir = tempdir().unwrap();
    let config = BridgeConfig {
        server: ServerConfig {
            bind_addr: "127.0.0.1".to_string(),
            port: 8090,
            api_key: None,
            cors_enabled: false,
            cors_origins: Vec::new(),
            concurrency_limit: 4,
            body_limit_bytes: 10 * 1024 * 1024,
            rate_limit: None,
            metrics_enabled: false,
        },
        storage: StorageConfig {
            data_dir: dir.path().to_path_buf(),
            media_ttl_days: 30,
        },
        transport: TransportConfig {
            tls_profile: "chrome".to_string(),
            proxy_url: None,
            timeout_secs: 5,
        },
        video: Default::default(),
    };
    let service = DefaultIdentityService::with_base_url(&config, Some(server.uri())).unwrap();
    service
        .import_credentials("__Secure-1PSID=psid; __Secure-1PSIDTS=psidts; SAPISID=sapisid")
        .await
        .unwrap();

    let bootstrap = service.bootstrap().await.unwrap();
    assert_eq!(bootstrap.bl, "boq_assistant-bard-web-server_20240101.00_p0");
    assert_eq!(bootstrap.snlm0e, "AIzaSyFakeTokenForTesting1234567890");
    assert_eq!(bootstrap.fsid, "1234567890123456789");
    assert_eq!(service.snapshot().await.status, SessionStatus::Valid);
}

#[tokio::test]
async fn apply_auth_headers_sets_expected_headers() {
    use gemini_bridge_config::{BridgeConfig, ServerConfig, StorageConfig, TransportConfig};
    use gemini_bridge_identity::{DefaultIdentityService, IdentityService};
    use http::HeaderMap;
    let _guard = ENV_LOCK.lock().await;
    unsafe { std::env::remove_var("BRIDGE_SECRET") };

    let dir = tempdir().unwrap();
    let config = BridgeConfig {
        server: ServerConfig {
            bind_addr: "127.0.0.1".to_string(),
            port: 8090,
            api_key: None,
            cors_enabled: false,
            cors_origins: Vec::new(),
            concurrency_limit: 4,
            body_limit_bytes: 10 * 1024 * 1024,
            rate_limit: None,
            metrics_enabled: false,
        },
        storage: StorageConfig {
            data_dir: dir.path().to_path_buf(),
            media_ttl_days: 30,
        },
        transport: TransportConfig {
            tls_profile: "chrome".to_string(),
            proxy_url: None,
            timeout_secs: 5,
        },
        video: Default::default(),
    };
    let service = DefaultIdentityService::new(&config).unwrap();
    let mut headers = HeaderMap::new();
    assert!(service.apply_auth_headers(&mut headers).is_err());

    service
        .import_credentials("__Secure-1PSID=psid; __Secure-1PSIDTS=psidts; SAPISID=sapisid")
        .await
        .unwrap();

    service.apply_auth_headers(&mut headers).unwrap();
    assert!(headers.contains_key(http::header::COOKIE));
    assert!(headers.contains_key(http::header::AUTHORIZATION));
}
