use std::sync::Mutex;
use tempfile::tempdir;

use gemini_bridge_identity::crypto::{decrypt, derive_key, encrypt};
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

// Serial lock for environment variable modifications
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_crypto_round_trip_with_secret() {
    let _guard = ENV_LOCK.lock().unwrap();
    // Safety: only one test touches BRIDGE_SECRET at a time via ENV_LOCK
    unsafe { std::env::set_var("BRIDGE_SECRET", "super-secret-key-12345") };

    let plaintext = b"{\"psid\":\"foo\",\"psidts\":\"bar\",\"sapisid\":\"baz\"}";
    let ciphertext = encrypt(plaintext).expect("encryption succeeds with key");
    assert_ne!(&ciphertext[..], plaintext);

    let decrypted = decrypt(&ciphertext).expect("decryption succeeds with same key");
    assert_eq!(decrypted, plaintext);

    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[test]
fn test_crypto_fails_with_wrong_key() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("BRIDGE_SECRET", "secret-key-A") };
    let plaintext = b"sensitive-data";
    let ciphertext = encrypt(plaintext).unwrap();

    // Switch key
    unsafe { std::env::set_var("BRIDGE_SECRET", "secret-key-B") };
    let result = decrypt(&ciphertext);
    assert!(result.is_none(), "decryption with wrong key must fail");

    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[test]
fn test_crypto_noop_when_secret_unset() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
    assert!(derive_key().is_none());
    assert!(encrypt(b"hello").is_none());
}

// ─── 3. Storage Tests ─────────────────────────────────────────────────────────

#[test]
fn test_storage_plaintext_round_trip() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::remove_var("BRIDGE_SECRET") };

    let dir = tempdir().unwrap();
    let json = r#"{"psid":"test_psid","psidts":"test_psidts","sapisid":"test_sapisid","imported_at":"2024-01-01T00:00:00Z"}"#;

    write_cookies(dir.path(), json).expect("write cookies");
    let read_back = read_cookies(dir.path()).expect("read cookies");
    assert_eq!(read_back.as_deref(), Some(json));
}

#[test]
fn test_storage_encrypted_round_trip() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("BRIDGE_SECRET", "my-encryption-password") };

    let dir = tempdir().unwrap();
    let json = r#"{"psid":"secure_psid","psidts":"secure_psidts","sapisid":"secure_sapisid","imported_at":"2024-01-01T00:00:00Z"}"#;

    write_cookies(dir.path(), json).expect("write encrypted cookies");

    // The file on disk should NOT be plaintext
    let raw_bytes = std::fs::read(dir.path().join("cookies.json")).unwrap();
    assert_ne!(raw_bytes, json.as_bytes());

    let read_back = read_cookies(dir.path()).expect("read decrypted cookies");
    assert_eq!(read_back.as_deref(), Some(json));

    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[test]
fn test_storage_encrypted_fails_with_wrong_key() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("BRIDGE_SECRET", "correct-key") };

    let dir = tempdir().unwrap();
    let json = r#"{"psid":"a","psidts":"b","sapisid":"c","imported_at":"2024-01-01T00:00:00Z"}"#;
    write_cookies(dir.path(), json).expect("write cookies");

    // Switch key and attempt read
    unsafe { std::env::set_var("BRIDGE_SECRET", "wrong-key") };
    let result = read_cookies(dir.path());
    assert!(result.is_err(), "reading with wrong key must fail closed");

    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[cfg(unix)]
#[test]
fn test_storage_creates_file_with_0600_mode() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::remove_var("BRIDGE_SECRET") };

    let dir = tempdir().unwrap();
    write_cookies(dir.path(), "{}").unwrap();

    let meta = std::fs::metadata(dir.path().join("cookies.json")).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "cookies.json must have 0600 mode on Unix");
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
async fn bootstrap_uses_imported_credentials_and_extracts_tokens() {
    use gemini_bridge_config::{BridgeConfig, ServerConfig, StorageConfig, TransportConfig};
    use gemini_bridge_identity::{DefaultIdentityService, IdentityService, SessionStatus};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    let dir = tempdir().unwrap();
    let config = BridgeConfig {
        server: ServerConfig {
            bind_addr: "127.0.0.1".to_string(),
            port: 8090,
            api_key: None,
            cors_enabled: false,
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
