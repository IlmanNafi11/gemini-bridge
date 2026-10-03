//! Local credential file storage with atomic, permission-safe writes.
//!
//! `cookies.json` is replaced through write-to-temp + fsync + atomic rename so
//! a failed write never truncates or corrupts an existing credential file.
//! When `BRIDGE_SECRET` is configured, the JSON is encrypted into the versioned
//! GBCK envelope (Argon2id KDF, random salt/nonce per write). On Unix the temp
//! file is created with `0600` permissions before any bytes are written. Legacy
//! SHA-256 ciphertext is read transparently and rewritten in the new envelope.

use std::path::{Path, PathBuf};

use crate::crypto;
use crate::error::IdentityError;

/// Read the cookie JSON from `data_dir/cookies.json`.
///
/// If `BRIDGE_SECRET` is set, the file must be encrypted (GBCK envelope or
/// legacy nonce-prefixed ciphertext) and is decrypted before returning. A
/// legacy-format file is returned only after it is durably rewritten in the
/// current envelope. Rewrap failure is observable and fails closed while the
/// atomic writer preserves the original ciphertext.
pub fn read_cookies(data_dir: &Path) -> Result<Option<String>, IdentityError> {
    let path = data_dir.join("cookies.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read(&path).map_err(|_| IdentityError::Storage)?;
    let encrypted = crypto::read_bridge_secret()?.is_some();
    let json_bytes = if encrypted {
        let plaintext = crypto::decrypt(&raw)?;
        if crypto::is_legacy_ciphertext(&raw) {
            let migrated = crypto::encrypt(&plaintext)
                .map_err(|_| IdentityError::LegacyCredentialRewrap)?
                .ok_or(IdentityError::LegacyCredentialRewrap)?;
            atomic_write(&path, data_dir, &migrated)
                .map_err(|_| IdentityError::LegacyCredentialRewrap)?;
        }
        plaintext
    } else {
        raw
    };
    String::from_utf8(json_bytes)
        .map(Some)
        .map_err(|_| IdentityError::Storage)
}

/// Atomically store cookie JSON in `data_dir/cookies.json` with 0600
/// permissions on Unix. A weak configured secret fails before touching either
/// the target file or live credential state.
pub fn write_cookies(data_dir: &Path, json: &str) -> Result<(), IdentityError> {
    std::fs::create_dir_all(data_dir).map_err(|_| IdentityError::Storage)?;
    let bytes = match crypto::encrypt(json.as_bytes())? {
        Some(encrypted) => encrypted,
        None => json.as_bytes().to_vec(),
    };
    atomic_write(&data_dir.join("cookies.json"), data_dir, &bytes)
}

fn atomic_write(target: &Path, parent: &Path, bytes: &[u8]) -> Result<(), IdentityError> {
    let (temp_path, mut file) = create_temporary_file(target)?;
    let write_result = (|| {
        use std::io::Write;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp_path, target)?;
        // Persist the rename where the platform supports syncing directories.
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok::<(), std::io::Error>(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
        return Err(IdentityError::Storage);
    }
    Ok(())
}

#[cfg(unix)]
fn create_temporary_file(target: &Path) -> Result<(PathBuf, std::fs::File), IdentityError> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = target.parent().ok_or(IdentityError::Storage)?;
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cookies.json");
    for _ in 0..64 {
        let temp_path = parent.join(format!(
            ".{file_name}.tmp.{}.{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(IdentityError::Storage),
        }
    }
    Err(IdentityError::Storage)
}

#[cfg(not(unix))]
fn create_temporary_file(target: &Path) -> Result<(PathBuf, std::fs::File), IdentityError> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = target.parent().ok_or(IdentityError::Storage)?;
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cookies.json");
    for _ in 0..64 {
        let temp_path = parent.join(format!(
            ".{file_name}.tmp.{}.{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(IdentityError::Storage),
        }
    }
    Err(IdentityError::Storage)
}
