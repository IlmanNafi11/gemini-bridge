//! Multi-tenant session vault backed by `sessions.sqlite`.
//!
//! Each client application owns one row: an opaque `ses_…` token (stored only as
//! a SHA-256 hash), an optional label, the encrypted cookie envelope, and the
//! lifecycle timestamps. Cookie material is written through the shared
//! [`crate::crypto`] GBCK envelope and never appears in `Debug` or error text.
//!
//! Upstream probing and per-session rotation build on this store in V.2; V.1
//! deliberately keeps the store synchronous and free of transport concerns.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::crypto;
use crate::error::IdentityError;
use crate::model::{SessionCredentials, SessionStatus};
use crate::session::rfc3339_now;

/// Random bytes backing a session token (spec §5: 32 bytes `OsRng`).
const TOKEN_BYTES: usize = 32;
/// Public prefix that makes session tokens self-identifying.
pub const TOKEN_PREFIX: &str = "ses_";

/// One tenant's vault row. Carries no secret material: the cookie envelope and
/// token hash stay inside the database.
pub struct VaultSession {
    pub id: String,
    pub label: Option<String>,
    pub status: SessionStatus,
    pub imported_at: String,
    pub updated_at: String,
}

impl std::fmt::Debug for VaultSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultSession")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("status", &self.status)
            .field("imported_at", &self.imported_at)
            .field("updated_at", &self.updated_at)
            .field("psid", &"***REDACTED***")
            .finish_non_exhaustive()
    }
}

/// Fields required to insert or replace a vault row.
pub struct NewVaultSession<'a> {
    pub id: &'a str,
    pub token_hash: &'a str,
    pub label: Option<&'a str>,
    pub credentials: &'a SessionCredentials,
    pub status: SessionStatus,
}

/// Generate a fresh opaque session token: `ses_` + 32 random bytes, URL-safe.
pub fn generate_session_token() -> String {
    use aes_gcm::aead::{OsRng, rand_core::RngCore};

    let mut bytes = [0_u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Hash a session token for storage. The raw token is never persisted.
pub fn hash_session_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Synchronous SQLite-backed session vault.
pub struct VaultStore {
    conn: Connection,
    path: PathBuf,
}

impl VaultStore {
    /// Open or create the vault database, applying the schema and hardening
    /// file permissions before any row is written.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IdentityError> {
        let path = path.as_ref().to_path_buf();
        secure_database_paths(&path)?;
        let mut conn = Connection::open(&path).map_err(vault_error)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(vault_error)?;
        crate::vault::apply_migrations(&mut conn)?;
        secure_database_files(&path)?;
        Ok(Self { conn, path })
    }

    /// Filesystem path of the backing database.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Insert a new row or atomically replace the row with the same id, keeping
    /// the caller's token hash, label, cookie envelope, and status.
    pub fn upsert_session(
        &mut self,
        input: NewVaultSession<'_>,
    ) -> Result<VaultSession, IdentityError> {
        let envelope = encrypt_credentials(input.credentials)?;
        let imported_at = if input.credentials.imported_at.is_empty() {
            rfc3339_now()
        } else {
            input.credentials.imported_at.clone()
        };
        let updated_at = rfc3339_now();

        self.conn
            .execute(
                "INSERT INTO sessions (id, token_hash, label, cookie_envelope, status, imported_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                     token_hash      = excluded.token_hash,
                     label           = excluded.label,
                     cookie_envelope = excluded.cookie_envelope,
                     status          = excluded.status,
                     imported_at     = excluded.imported_at,
                     updated_at      = excluded.updated_at",
                params![
                    input.id,
                    input.token_hash,
                    input.label,
                    envelope,
                    status_to_str(input.status),
                    imported_at,
                    updated_at,
                ],
            )
            .map_err(vault_error)?;

        Ok(VaultSession {
            id: input.id.to_owned(),
            label: input.label.map(str::to_owned),
            status: input.status,
            imported_at,
            updated_at,
        })
    }

    /// Fetch a row by session id.
    pub fn get(&self, id: &str) -> Result<Option<VaultSession>, IdentityError> {
        self.conn
            .query_row(
                "SELECT id, label, status, imported_at, updated_at FROM sessions WHERE id = ?1",
                params![id],
                row_to_session,
            )
            .optional()
            .map_err(vault_error)
    }

    /// Resolve the owning session from a stored token hash.
    pub fn find_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<VaultSession>, IdentityError> {
        self.conn
            .query_row(
                "SELECT id, label, status, imported_at, updated_at
                   FROM sessions WHERE token_hash = ?1",
                params![token_hash],
                row_to_session,
            )
            .optional()
            .map_err(vault_error)
    }

    /// All session ids currently in the vault, ordered for stable reporting.
    pub fn session_ids(&self) -> Result<Vec<String>, IdentityError> {
        let mut statement = self
            .conn
            .prepare("SELECT id FROM sessions ORDER BY imported_at ASC, id ASC")
            .map_err(vault_error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(vault_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(vault_error)
    }

    /// Number of rows in the vault.
    pub fn session_rows(&self) -> Result<usize, IdentityError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .map_err(vault_error)?;
        Ok(count as usize)
    }

    /// Decrypt and return the credentials held for `id`.
    ///
    /// Fails closed when the envelope needs a `BRIDGE_SECRET` that is missing or
    /// wrong, instead of degrading to plaintext or partial credentials.
    pub fn session_cookies(&self, id: &str) -> Result<Option<SessionCredentials>, IdentityError> {
        let envelope: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT cookie_envelope FROM sessions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(vault_error)?;

        let Some(envelope) = envelope else {
            return Ok(None);
        };
        decrypt_credentials(&envelope).map(Some)
    }

    /// Replace the stored cookie envelope (used by per-session rotation).
    pub fn update_cookies(
        &mut self,
        id: &str,
        credentials: &SessionCredentials,
    ) -> Result<bool, IdentityError> {
        let envelope = encrypt_credentials(credentials)?;
        let affected = self
            .conn
            .execute(
                "UPDATE sessions
                    SET cookie_envelope = ?1, imported_at = ?2, updated_at = ?3
                  WHERE id = ?4",
                params![envelope, credentials.imported_at, rfc3339_now(), id],
            )
            .map_err(vault_error)?;
        Ok(affected > 0)
    }

    /// Move a row to a new lifecycle status.
    pub fn update_status(
        &mut self,
        id: &str,
        status: SessionStatus,
    ) -> Result<bool, IdentityError> {
        let affected = self
            .conn
            .execute(
                "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status_to_str(status), rfc3339_now(), id],
            )
            .map_err(vault_error)?;
        Ok(affected > 0)
    }

    /// Purge a row. Reports whether a row was actually removed so the caller can
    /// distinguish a real delete from an unknown id.
    pub fn delete(&mut self, id: &str) -> Result<bool, IdentityError> {
        let affected = self
            .conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])
            .map_err(vault_error)?;
        Ok(affected > 0)
    }
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<VaultSession> {
    let status: String = row.get(2)?;
    Ok(VaultSession {
        id: row.get(0)?,
        label: row.get(1)?,
        status: status_from_str(&status),
        imported_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

/// Persisted status encoding: stable snake_case so rows survive renames.
fn status_to_str(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Unconfigured => "unconfigured",
        SessionStatus::Valid => "valid",
        SessionStatus::Stale => "stale",
        SessionStatus::NeedsReauth => "needs_reauth",
        SessionStatus::IpFlagged => "ip_flagged",
    }
}

/// Unknown persisted values are treated as `NeedsReauth`: a row we cannot prove
/// healthy must never be handed to the upstream as valid.
fn status_from_str(value: &str) -> SessionStatus {
    match value {
        "valid" => SessionStatus::Valid,
        "stale" => SessionStatus::Stale,
        "ip_flagged" => SessionStatus::IpFlagged,
        "unconfigured" => SessionStatus::Unconfigured,
        _ => SessionStatus::NeedsReauth,
    }
}

fn encrypt_credentials(credentials: &SessionCredentials) -> Result<Vec<u8>, IdentityError> {
    let json = serde_json::to_vec(credentials).map_err(|_| IdentityError::Storage)?;
    Ok(crypto::encrypt(&json)?.unwrap_or(json))
}

fn decrypt_credentials(envelope: &[u8]) -> Result<SessionCredentials, IdentityError> {
    let encryption_enabled = crypto::read_bridge_secret()?.is_some();
    let plaintext = if encryption_enabled || !crypto::is_legacy_ciphertext(envelope) {
        crypto::decrypt(envelope)?
    } else {
        envelope.to_vec()
    };
    serde_json::from_slice(&plaintext).map_err(|_| IdentityError::Storage)
}

fn vault_error(error: rusqlite::Error) -> IdentityError {
    IdentityError::Vault(error.to_string())
}

const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id              TEXT PRIMARY KEY,
    token_hash      TEXT NOT NULL UNIQUE,
    label           TEXT,
    cookie_envelope BLOB NOT NULL,
    status          TEXT NOT NULL,
    imported_at     TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);
"#;

/// Apply the vault schema. Kept idempotent so `open` is safe on every startup.
pub(crate) fn apply_migrations(conn: &mut Connection) -> Result<(), IdentityError> {
    conn.execute_batch(MIGRATION_V1).map_err(vault_error)
}

#[cfg(unix)]
fn secure_database_paths(path: &Path) -> Result<(), IdentityError> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let ancestors = parent.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(ancestors).map_err(|_| IdentityError::Storage)?;
    match std::fs::DirBuilder::new().mode(0o700).create(parent) {
        Ok(()) => std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| IdentityError::Storage)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(IdentityError::Storage),
    }
    secure_database_files(path)?;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|_| IdentityError::Storage)?;
    secure_database_files(path)
}

#[cfg(unix)]
fn secure_database_files(path: &Path) -> Result<(), IdentityError> {
    use std::os::unix::fs::PermissionsExt;

    for file in database_and_sidecars(path) {
        if file.exists() {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| IdentityError::Storage)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_database_paths(path: &Path) -> Result<(), IdentityError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|_| IdentityError::Storage)
}

#[cfg(not(unix))]
fn secure_database_files(_path: &Path) -> Result<(), IdentityError> {
    Ok(())
}

fn database_and_sidecars(path: &Path) -> [PathBuf; 4] {
    let mut journal = path.as_os_str().to_owned();
    journal.push("-journal");
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let mut shm = path.as_os_str().to_owned();
    shm.push("-shm");
    [path.to_path_buf(), journal.into(), wal.into(), shm.into()]
}
