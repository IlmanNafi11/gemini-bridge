//! (validated lifecycle and per-session rotation).
//!
//! V.1 covers the vault as a synchronous store: on-disk shape, schema, CRUD,
//! token hashing, encrypted cookie envelope, fail-closed key handling, and
//! secret-safe `Debug`. V.2 covers upstream probing (create preserves the
//! prior row on failure; validated replace is atomic) and per-session
//! single-flight rotation (rotated `__Secure-1PSIDTS` persisted for the owning
//! session only; failure flips that row; `sorry/` flips to `ip_flagged`).
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::tempdir;

use gemini_bridge_identity::vault::{
    NewVaultSession, VaultStore, generate_session_token, hash_session_token,
};
use gemini_bridge_identity::{SessionCredentials, SessionStatus};

/// Serializes tests that mutate the process-wide `BRIDGE_SECRET`.
static ENV_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

const SECRET_A: &str = "correct-horse-battery-staple-bridge-A";
const SECRET_B: &str = "correct-horse-battery-staple-bridge-B";

fn credentials(psidts: &str) -> SessionCredentials {
    SessionCredentials {
        psid: "psid-a".to_owned(),
        psidts: psidts.to_owned(),
        sapisid: "sapisid-a".to_owned(),
        imported_at: rfc3339_fixture(),
    }
}

fn rfc3339_fixture() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

fn with_secret<T>(secret: Option<&str>, body: impl FnOnce() -> T) -> T {
    let _guard = ENV_LOCK.blocking_lock();
    match secret {
        Some(value) => unsafe { std::env::set_var("BRIDGE_SECRET", value) },
        None => unsafe { std::env::remove_var("BRIDGE_SECRET") },
    }
    let result = body();
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
    result
}

fn open(dir: &std::path::Path) -> VaultStore {
    VaultStore::open(dir.join("sessions.sqlite")).expect("open vault")
}

// ─── V.2: validated lifecycle & per-session rotation ────────────────────────

/// Required by every V.2 test: service construction validates `BRIDGE_SECRET`
/// and the store is created through it, so each test holds the env lock.
async fn v2_service(
    dir: &std::path::Path,
    base_url: Option<String>,
) -> gemini_bridge_identity::VaultService {
    use gemini_bridge_config::{BridgeConfig, ServerConfig, StorageConfig, TransportConfig};

    let server = ServerConfig {
        bind_addr: "127.0.0.1".into(),
        port: 8090,
        api_key: None,
        cors_enabled: false,
        cors_origins: Vec::new(),
        concurrency_limit: 4,
        body_limit_bytes: 10 * 1024 * 1024,
        rate_limit: None,
        metrics_enabled: false,
    };
    let storage = StorageConfig {
        data_dir: dir.to_path_buf(),
        media_ttl_days: 30,
    };
    let transport = TransportConfig {
        tls_profile: "chrome".into(),
        proxy_url: None,
        timeout_secs: 5,
        stream_idle_timeout_secs: 30,
    };
    let config = BridgeConfig {
        server,
        storage,
        transport,
        video: Default::default(),
    };
    gemini_bridge_identity::VaultService::new(&config, base_url).unwrap()
}

/// Seed a valid session row for the V.2 tests without an upstream call.
async fn seed_session(
    service: &gemini_bridge_identity::VaultService,
    id: &str,
    token: &str,
    label: Option<&str>,
    cookie: &str,
) {
    let credentials =
        gemini_bridge_identity::session::parse_cookie_header(cookie).expect("test cookie parses");
    service
        .store()
        .upsert_session(gemini_bridge_identity::NewVaultSession {
            id,
            token_hash: &gemini_bridge_identity::hash_session_token(token),
            label,
            credentials: &credentials,
            status: SessionStatus::Valid,
        })
        .unwrap();
}

#[tokio::test]
async fn validated_create_persists_only_after_probe_success() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _guard = ENV_LOCK.lock().await;
    unsafe { std::env::set_var("BRIDGE_SECRET", SECRET_A) };

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_GEMINI_HTML))
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempdir().unwrap();
    let service = v2_service(dir.path(), Some(server.uri())).await;
    let created = service
        .create_validated("ses_probe", "tok-probe", Some("client a"), COOKIE_OK)
        .await
        .unwrap();

    assert_eq!(created.id, "ses_probe");
    assert_eq!(created.status, SessionStatus::Valid);
    assert_eq!(service.store().session_rows().unwrap(), 1);
    let cookies = service
        .store()
        .session_cookies("ses_probe")
        .unwrap()
        .unwrap();
    assert_eq!(cookies.psidts, "ts-a");
    assert_eq!(created.imported_at, cookies.imported_at);

    // Probe must have hit upstream exactly once before persisting.
    server.verify().await;
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[tokio::test]
async fn failed_probe_leaves_prior_row_untouched() {
    use gemini_bridge_identity::IdentityError;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _guard = ENV_LOCK.lock().await;
    unsafe { std::env::set_var("BRIDGE_SECRET", SECRET_A) };

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempdir().unwrap();
    let service = v2_service(dir.path(), Some(server.uri())).await;
    service
        .store()
        .upsert_session(gemini_bridge_identity::NewVaultSession {
            id: "ses_keep",
            token_hash: &gemini_bridge_identity::hash_session_token("tok-keep"),
            label: None,
            credentials: &credentials("ts-a"),
            status: SessionStatus::Valid,
        })
        .unwrap();
    let before = service.store().get("ses_keep").unwrap().unwrap();

    let error = service
        .create_validated("ses_keep", "tok-new", Some("client b"), COOKIE_BAD)
        .await
        .unwrap_err();
    assert!(
        matches!(error, IdentityError::NeedsReauth),
        "probe failure must surface NeedsReauth: {error}"
    );

    // The prior row must be byte-identical: old token resolves, new token does
    // not, credentials unchanged, row count unchanged.
    let after = service.store().get("ses_keep").unwrap().unwrap();
    assert_eq!(after.id, before.id);
    assert_eq!(after.status, before.status);
    assert!(
        service
            .store()
            .find_by_token_hash(&gemini_bridge_identity::hash_session_token("tok-keep"))
            .unwrap()
            .is_some(),
        "old token hash must still resolve"
    );
    assert!(
        service
            .store()
            .find_by_token_hash(&gemini_bridge_identity::hash_session_token("tok-new"))
            .unwrap()
            .is_none(),
        "failed probe must not swap the token hash"
    );
    assert_eq!(
        service
            .store()
            .session_cookies("ses_keep")
            .unwrap()
            .unwrap()
            .psidts,
        "ts-a",
        "credentials must be unchanged after a failed probe"
    );
    assert_eq!(service.store().session_rows().unwrap(), 1);
    server.verify().await;
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[tokio::test]
async fn validated_replace_swaps_credentials_and_token_atomically() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _guard = ENV_LOCK.lock().await;
    unsafe { std::env::set_var("BRIDGE_SECRET", SECRET_A) };

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_GEMINI_HTML))
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempdir().unwrap();
    let service = v2_service(dir.path(), Some(server.uri())).await;
    service
        .store()
        .upsert_session(gemini_bridge_identity::NewVaultSession {
            id: "ses_swap",
            token_hash: &gemini_bridge_identity::hash_session_token("tok-old"),
            label: Some("old label"),
            credentials: &credentials("ts-a"),
            status: SessionStatus::Valid,
        })
        .unwrap();
    let replaced = service
        .create_validated("ses_swap", "tok-new", Some("new label"), COOKIE_OK2)
        .await
        .unwrap();
    assert_eq!(replaced.label.as_deref(), Some("new label"));
    assert_eq!(replaced.status, SessionStatus::Valid);

    assert_eq!(
        service.store().session_rows().unwrap(),
        1,
        "replace must not add a row"
    );
    assert!(
        service
            .store()
            .find_by_token_hash(&gemini_bridge_identity::hash_session_token("tok-old"))
            .unwrap()
            .is_none(),
        "old token hash must stop resolving after replace"
    );
    assert!(
        service
            .store()
            .find_by_token_hash(&gemini_bridge_identity::hash_session_token("tok-new"))
            .unwrap()
            .is_some(),
        "new token hash must resolve after replace"
    );
    let cookies = service
        .store()
        .session_cookies("ses_swap")
        .unwrap()
        .unwrap();
    assert_eq!(cookies.psidts, "ts-b", "swapped credentials must persist");
    server.verify().await;
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[tokio::test]
async fn rotation_success_persists_rotated_cookie_for_owning_session_only() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _guard = ENV_LOCK.lock().await;
    unsafe { std::env::set_var("BRIDGE_SECRET", SECRET_A) };

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .and(header("cookie", COOKIE_OK))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SAMPLE_GEMINI_HTML)
                .insert_header(
                    "set-cookie",
                    "__Secure-1PSIDTS=ts-rotated; Path=/; Secure; HttpOnly",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempdir().unwrap();
    let service = v2_service(dir.path(), Some(server.uri())).await;
    seed_session(&service, "ses_rot", "tok-rot", Some("owner"), COOKIE_OK).await;
    seed_session(&service, "ses_other", "tok-other", None, COOKIE_OK).await;

    service
        .rotate_session("ses_rot")
        .await
        .expect("rotation must succeed");

    // Owning session now carries the rotated cookie and is Valid.
    let rotated = service.store().session_cookies("ses_rot").unwrap().unwrap();
    assert_eq!(rotated.psidts, "ts-rotated");
    assert_eq!(
        service.store().get("ses_rot").unwrap().unwrap().status,
        SessionStatus::Valid
    );
    // The unrelated session is untouched.
    let untouched = service
        .store()
        .session_cookies("ses_other")
        .unwrap()
        .unwrap();
    assert_eq!(untouched.psidts, "ts-a");
    assert_eq!(
        service.store().get("ses_other").unwrap().unwrap().status,
        SessionStatus::Valid
    );
    server.verify().await;
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[tokio::test]
async fn concurrent_rotation_is_single_flight_per_session() {
    use std::sync::Arc;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _guard = ENV_LOCK.lock().await;
    unsafe { std::env::set_var("BRIDGE_SECRET", SECRET_A) };

    let server = MockServer::start().await;
    // Session A: two concurrent rotations must coalesce into one `/app` call.
    Mock::given(method("GET"))
        .and(path("/app"))
        .and(header("cookie", COOKIE_OK))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SAMPLE_GEMINI_HTML)
                .insert_header(
                    "set-cookie",
                    "__Secure-1PSIDTS=ts-coalesced; Path=/; Secure",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    // Session B rotates concurrently: one distinct `/app` call of its own.
    Mock::given(method("GET"))
        .and(path("/app"))
        .and(header("cookie", COOKIE_OK2))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SAMPLE_GEMINI_HTML)
                .insert_header("set-cookie", "__Secure-1PSIDTS=ts-b-rot; Path=/; Secure"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempdir().unwrap();
    let service = Arc::new(v2_service(dir.path(), Some(server.uri())).await);
    {
        seed_session(&service, "ses_a", "tok-a", Some("a"), COOKIE_OK).await;
    }
    {
        seed_session(&service, "ses_b", "tok-b", Some("b"), COOKIE_OK2).await;
    }

    let (sa, sb, s1) = (
        Arc::clone(&service),
        Arc::clone(&service),
        Arc::clone(&service),
    );
    let (ra, rb, rs1) = tokio::join!(
        sa.rotate_session("ses_a"),
        sb.rotate_session("ses_b"),
        s1.rotate_session("ses_a"),
    );
    for result in [ra, rb, rs1] {
        result.expect("all rotations must succeed");
    }

    // Exactly two upstream calls total: one per session, not per caller.
    server.verify().await;
    assert_eq!(
        service
            .store()
            .session_cookies("ses_a")
            .unwrap()
            .unwrap()
            .psidts,
        "ts-coalesced"
    );
    assert_eq!(
        service
            .store()
            .session_cookies("ses_b")
            .unwrap()
            .unwrap()
            .psidts,
        "ts-b-rot"
    );
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[tokio::test]
async fn rotation_failure_marks_owning_row_needs_reauth() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _guard = ENV_LOCK.lock().await;
    unsafe { std::env::set_var("BRIDGE_SECRET", SECRET_A) };

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .and(header("cookie", COOKIE_OK))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempdir().unwrap();
    let service = v2_service(dir.path(), Some(server.uri())).await;
    seed_session(&service, "ses_dead", "tok-dead", None, COOKIE_OK).await;
    seed_session(&service, "ses_ok", "tok-ok", None, COOKIE_OK).await;

    let error = service.rotate_session("ses_dead").await.unwrap_err();
    assert!(
        matches!(error, gemini_bridge_identity::IdentityError::NeedsReauth),
        "rotation failure must surface NeedsReauth: {error}"
    );
    assert_eq!(
        service.store().get("ses_dead").unwrap().unwrap().status,
        SessionStatus::NeedsReauth,
        "the owning row must flip to needs_reauth"
    );
    assert_eq!(
        service.store().get("ses_ok").unwrap().unwrap().status,
        SessionStatus::Valid,
        "an unrelated session must not be affected"
    );
    // The dead row's cookie is unchanged (no fake rotated value).
    assert_eq!(
        service
            .store()
            .session_cookies("ses_dead")
            .unwrap()
            .unwrap()
            .psidts,
        "ts-a"
    );
    server.verify().await;
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[tokio::test]
async fn sorry_redirect_marks_owning_row_ip_flagged() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _guard = ENV_LOCK.lock().await;
    unsafe { std::env::set_var("BRIDGE_SECRET", SECRET_A) };

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .and(header("cookie", COOKIE_OK))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "https://www.google.com/sorry/index"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempdir().unwrap();
    let service = v2_service(dir.path(), Some(server.uri())).await;
    seed_session(&service, "ses_flagged", "tok-flag", None, COOKIE_OK).await;
    seed_session(&service, "ses_clean", "tok-clean", None, COOKIE_OK).await;

    let error = service.rotate_session("ses_flagged").await.unwrap_err();
    assert!(
        matches!(error, gemini_bridge_identity::IdentityError::IpFlagged),
        "sorry/ redirect must surface IpFlagged: {error}"
    );
    assert_eq!(
        service.store().get("ses_flagged").unwrap().unwrap().status,
        SessionStatus::IpFlagged,
        "the owning row must flip to ip_flagged"
    );
    assert_eq!(
        service.store().get("ses_clean").unwrap().unwrap().status,
        SessionStatus::Valid,
        "an unrelated session must not be affected"
    );
    server.verify().await;
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

#[tokio::test]
async fn rotation_of_unknown_session_fails_without_upstream_call() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _guard = ENV_LOCK.lock().await;
    unsafe { std::env::set_var("BRIDGE_SECRET", SECRET_A) };

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_GEMINI_HTML))
        .expect(0)
        .mount(&server)
        .await;

    let dir = tempdir().unwrap();
    let service = v2_service(dir.path(), Some(server.uri())).await;
    let error = service.rotate_session("ses_missing").await.unwrap_err();
    assert!(
        matches!(
            error,
            gemini_bridge_identity::IdentityError::MissingCredentials
        ),
        "unknown session id must surface MissingCredentials, never a global fallback: {error}"
    );
    server.verify().await;
    unsafe { std::env::remove_var("BRIDGE_SECRET") };
}

// ─── V.2 fixtures ───────────────────────────────────────────────────────────

const COOKIE_OK: &str = "__Secure-1PSID=psid-a; __Secure-1PSIDTS=ts-a; SAPISID=sapisid-a";
const COOKIE_OK2: &str = "__Secure-1PSID=psid-b; __Secure-1PSIDTS=ts-b; SAPISID=sapisid-b";
const COOKIE_BAD: &str = "__Secure-1PSID=psid-bad; __Secure-1PSIDTS=ts-bad; SAPISID=sapisid-bad";
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

// ─── On-disk shape & permissions ─────────────────────────────────────────────

#[test]
fn creates_sessions_sqlite_with_restrictive_permissions() {
    with_secret(Some(SECRET_A), || {
        let dir = tempdir().unwrap();
        let _store = open(dir.path());

        let db_path = dir.path().join("sessions.sqlite");
        assert!(
            db_path.exists(),
            "sessions.sqlite must be created at data_dir"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&db_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "vault database must stay private to the owner");
        }
    });
}

#[test]
fn schema_holds_every_documented_column() {
    with_secret(Some(SECRET_A), || {
        let dir = tempdir().unwrap();
        let mut store = open(dir.path());
        store
            .upsert_session(NewVaultSession {
                id: "ses_schema",
                token_hash: &hash_session_token("token-schema"),
                label: Some("client schema"),
                credentials: &credentials("ts-schema"),
                status: SessionStatus::Valid,
            })
            .unwrap();
        drop(store);

        let connection = rusqlite::Connection::open(dir.path().join("sessions.sqlite")).unwrap();
        let mut statement = connection.prepare("PRAGMA table_info(sessions)").unwrap();
        let columns: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        for expected in [
            "id",
            "token_hash",
            "label",
            "cookie_envelope",
            "status",
            "imported_at",
            "updated_at",
        ] {
            assert!(
                columns.iter().any(|column| column == expected),
                "schema must include {expected:?}; got {columns:?}"
            );
        }
    });
}

// ─── Token hashing ───────────────────────────────────────────────────────────

#[test]
fn session_token_is_prefixed_random_and_stable_under_hashing() {
    let first = generate_session_token();
    let second = generate_session_token();

    assert!(
        first.starts_with("ses_"),
        "token needs the ses_ prefix: {first}"
    );
    assert_ne!(first, second, "each token must be unique");
    assert!(first.len() > "ses_".len());

    assert_eq!(hash_session_token(&first), hash_session_token(&first));
    assert_ne!(
        hash_session_token(&first),
        hash_session_token(&second),
        "distinct tokens must hash distinctly"
    );
    assert!(
        !hash_session_token(&first).contains(&first[4..]),
        "the stored hash must not embed the raw token"
    );
}

// ─── CRUD round-trip ─────────────────────────────────────────────────────────

#[test]
fn create_read_update_delete_round_trip() {
    with_secret(Some(SECRET_A), || {
        let dir = tempdir().unwrap();
        let mut store = open(dir.path());

        let raw_token = generate_session_token();
        let created = store
            .upsert_session(NewVaultSession {
                id: "ses_one",
                token_hash: &hash_session_token(&raw_token),
                label: Some("client one"),
                credentials: &credentials("ts-one"),
                status: SessionStatus::Valid,
            })
            .unwrap();

        assert_eq!(created.id, "ses_one");
        assert_eq!(created.label.as_deref(), Some("client one"));
        assert_eq!(created.status, SessionStatus::Valid);
        assert!(
            !created.imported_at.is_empty(),
            "imported_at must be recorded"
        );
        assert!(
            !created.updated_at.is_empty(),
            "updated_at must be recorded"
        );

        // Read by id and by token hash resolve to the same tenant.
        let by_id = store.get("ses_one").unwrap().expect("row by id");
        assert_eq!(by_id.id, "ses_one");
        let by_token = store
            .find_by_token_hash(&hash_session_token(&raw_token))
            .unwrap()
            .expect("row by token hash");
        assert_eq!(by_token.id, by_id.id);

        // The encrypted envelope round-trips back to the parsed credentials.
        let cookies = store
            .session_cookies("ses_one")
            .unwrap()
            .expect("cookie envelope");
        assert_eq!(cookies.psid, "psid-a");
        assert_eq!(cookies.psidts, "ts-one");
        assert_eq!(cookies.sapisid, "sapisid-a");

        // Update: rotated cookie plus status transition persist.
        assert!(
            store
                .update_cookies("ses_one", &credentials("ts-rotated"))
                .unwrap(),
            "updating a live row reports a change"
        );
        assert_eq!(
            store.session_cookies("ses_one").unwrap().unwrap().psidts,
            "ts-rotated"
        );
        assert!(
            store
                .update_status("ses_one", SessionStatus::NeedsReauth)
                .unwrap(),
            "status transition on a live row reports a change"
        );
        assert_eq!(
            store.get("ses_one").unwrap().unwrap().status,
            SessionStatus::NeedsReauth
        );

        // Delete removes the row and invalidates the token.
        assert!(
            store.delete("ses_one").unwrap(),
            "delete must report a removed row"
        );
        assert!(store.get("ses_one").unwrap().is_none(), "row must be gone");
        assert!(
            store
                .find_by_token_hash(&hash_session_token(&raw_token))
                .unwrap()
                .is_none(),
            "deleted token must no longer resolve"
        );
        assert!(
            !store.delete("ses_one").unwrap(),
            "second delete removes nothing"
        );
    });
}

#[test]
fn upsert_replaces_the_existing_row_for_the_same_id() {
    with_secret(Some(SECRET_A), || {
        let dir = tempdir().unwrap();
        let mut store = open(dir.path());

        store
            .upsert_session(NewVaultSession {
                id: "ses_replace",
                token_hash: &hash_session_token("old-token"),
                label: Some("old label"),
                credentials: &credentials("ts-old"),
                status: SessionStatus::Valid,
            })
            .unwrap();

        let replacement = store
            .upsert_session(NewVaultSession {
                id: "ses_replace",
                token_hash: &hash_session_token("new-token"),
                label: Some("new label"),
                credentials: &credentials("ts-new"),
                status: SessionStatus::Valid,
            })
            .unwrap();

        assert_eq!(replacement.label.as_deref(), Some("new label"));
        assert_eq!(
            store.session_rows().unwrap(),
            1,
            "replace must not add a row"
        );
        assert_eq!(
            store
                .session_cookies("ses_replace")
                .unwrap()
                .unwrap()
                .psidts,
            "ts-new"
        );
        assert!(
            store
                .find_by_token_hash(&hash_session_token("old-token"))
                .unwrap()
                .is_none(),
            "the previous token hash must stop resolving"
        );
        assert!(
            store
                .find_by_token_hash(&hash_session_token("new-token"))
                .unwrap()
                .is_some(),
            "the replacement token hash must resolve"
        );
    });
}

#[test]
fn token_hash_lookup_returns_none_for_unknown_tokens() {
    with_secret(Some(SECRET_A), || {
        let dir = tempdir().unwrap();
        let mut store = open(dir.path());
        store
            .upsert_session(NewVaultSession {
                id: "ses_two",
                token_hash: &hash_session_token("known-token"),
                label: None,
                credentials: &credentials("ts-two"),
                status: SessionStatus::Valid,
            })
            .unwrap();

        assert!(
            store
                .find_by_token_hash(&hash_session_token("known-token"))
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .find_by_token_hash(&hash_session_token("unknown-token"))
                .unwrap()
                .is_none()
        );
        assert!(
            !store
                .update_status("ses_absent", SessionStatus::Valid)
                .unwrap(),
            "updating a missing row reports no change"
        );
    });
}

// ─── Encryption & fail-closed key handling ───────────────────────────────────

#[test]
fn cookie_envelope_is_encrypted_at_rest_when_secret_is_set() {
    with_secret(Some(SECRET_A), || {
        let dir = tempdir().unwrap();
        let mut store = open(dir.path());
        store
            .upsert_session(NewVaultSession {
                id: "ses_enc",
                token_hash: &hash_session_token("token-enc"),
                label: None,
                credentials: &credentials("ts-enc"),
                status: SessionStatus::Valid,
            })
            .unwrap();
        drop(store);

        let raw = std::fs::read(dir.path().join("sessions.sqlite")).unwrap();
        for needle in [b"psid-a".as_slice(), b"sapisid-a".as_slice()] {
            assert!(
                !raw.windows(needle.len()).any(|window| window == needle),
                "cookie values must not appear in plaintext in the vault file"
            );
        }
        assert!(
            raw.windows(4).any(|window| window == b"GBCK"),
            "the stored envelope must use the versioned GBCK format"
        );
    });
}

#[test]
fn wrong_bridge_secret_fails_closed_without_disclosure() {
    let dir = tempdir().unwrap();

    with_secret(Some(SECRET_A), || {
        let mut store = open(dir.path());
        store
            .upsert_session(NewVaultSession {
                id: "ses_key",
                token_hash: &hash_session_token("token-key"),
                label: None,
                credentials: &credentials("ts-key"),
                status: SessionStatus::Valid,
            })
            .unwrap();
    });

    with_secret(Some(SECRET_B), || {
        let store = open(dir.path());
        let error = store.session_cookies("ses_key").unwrap_err();
        let rendered = format!("{error:?}: {error}");
        for secret in [SECRET_A, SECRET_B, "psid-a", "sapisid-a"] {
            assert!(
                !rendered.contains(secret),
                "error must not reveal {secret:?}: {rendered}"
            );
        }
    });
}

#[test]
fn encrypted_vault_without_bridge_secret_fails_closed() {
    let dir = tempdir().unwrap();

    with_secret(Some(SECRET_A), || {
        let mut store = open(dir.path());
        store
            .upsert_session(NewVaultSession {
                id: "ses_missing_key",
                token_hash: &hash_session_token("token-missing"),
                label: None,
                credentials: &credentials("ts-missing"),
                status: SessionStatus::Valid,
            })
            .unwrap();
    });

    with_secret(None, || {
        let store = open(dir.path());
        assert!(
            store.session_cookies("ses_missing_key").is_err(),
            "an encrypted vault must never silently degrade to plaintext"
        );
    });
}

// ─── Secret-safe Debug ───────────────────────────────────────────────────────

#[test]
fn vault_session_debug_never_prints_secret_material() {
    with_secret(Some(SECRET_A), || {
        let dir = tempdir().unwrap();
        let mut store = open(dir.path());
        let raw_token = generate_session_token();
        store
            .upsert_session(NewVaultSession {
                id: "ses_debug",
                token_hash: &hash_session_token(&raw_token),
                label: Some("labelled"),
                credentials: &credentials("ts-debug"),
                status: SessionStatus::Valid,
            })
            .unwrap();

        let session = store.get("ses_debug").unwrap().unwrap();
        let rendered = format!("{session:?}");

        for secret in [
            "psid-a".to_owned(),
            "sapisid-a".to_owned(),
            raw_token.clone(),
            hash_session_token(&raw_token),
            SECRET_A.to_owned(),
        ] {
            assert!(
                !rendered.contains(&secret),
                "Debug output must not contain {secret:?}: {rendered}"
            );
        }
        assert!(
            rendered.contains("ses_debug"),
            "non-secret identity stays visible"
        );
        assert!(
            rendered.contains("labelled"),
            "non-secret label stays visible"
        );
    });
}
