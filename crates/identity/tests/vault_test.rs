//! VaultStore unit tests — V.1 slice (`sessions.sqlite` core).
//!
//! The vault is exercised as a synchronous store: on-disk shape, schema, CRUD,
//! token hashing, encrypted cookie envelope, fail-closed key handling, and
//! secret-safe `Debug`. Upstream probing and rotation belong to V.2.

use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::tempdir;

use gemini_bridge_identity::vault::{
    NewVaultSession, VaultStore, generate_session_token, hash_session_token,
};
use gemini_bridge_identity::{SessionCredentials, SessionStatus};

/// Serializes tests that mutate the process-wide `BRIDGE_SECRET`.
static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

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
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
