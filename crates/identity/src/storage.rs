use std::path::Path;

use crate::error::IdentityError;

/// Read the cookie JSON from `data_dir/cookies.json`.
///
/// If `BRIDGE_SECRET` is set, the file is expected to be AES-GCM encrypted
/// (nonce-prefixed) and is decrypted before returning.
pub fn read_cookies(data_dir: &Path) -> Result<Option<String>, IdentityError> {
    let path = data_dir.join("cookies.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read(&path).map_err(|_| IdentityError::Storage)?;
    let json_bytes = if crate::crypto::derive_key().is_some() {
        crate::crypto::decrypt(&raw).ok_or(IdentityError::Storage)?
    } else {
        raw
    };
    let s = String::from_utf8(json_bytes).map_err(|_| IdentityError::Storage)?;
    Ok(Some(s))
}

/// Write the cookie JSON to `data_dir/cookies.json` with 0600 permissions.
///
/// If `BRIDGE_SECRET` is set, the JSON is AES-GCM encrypted before write.
pub fn write_cookies(data_dir: &Path, json: &str) -> Result<(), IdentityError> {
    std::fs::create_dir_all(data_dir).map_err(|_| IdentityError::Storage)?;
    let path = data_dir.join("cookies.json");

    let bytes: Vec<u8> = if let Some(encrypted) = crate::crypto::encrypt(json.as_bytes()) {
        encrypted
    } else {
        json.as_bytes().to_vec()
    };

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| IdentityError::Storage)?;
        file.write_all(&bytes).map_err(|_| IdentityError::Storage)?;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms).map_err(|_| IdentityError::Storage)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, &bytes).map_err(|_| IdentityError::Storage)?;
    }

    Ok(())
}
