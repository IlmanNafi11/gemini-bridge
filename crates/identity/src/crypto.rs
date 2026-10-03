//! Cookie-at-rest cryptography for the identity store.
//!
//! New ciphertext is stored in a versioned `GBCK` envelope containing a fresh
//! random Argon2id salt and AES-GCM nonce. Legacy nonce-prefixed ciphertexts
//! from the original unsalted SHA-256 derivation remain readable and are
//! transparently migrated by the storage layer after decryption.

use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, rand_core::RngCore},
};
use argon2::Argon2;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::error::IdentityError;

const MAGIC: &[u8; 4] = b"GBCK";
const FORMAT_ARGON2ID: u8 = 0x01;
const HEADER_LEN: usize = 5;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const MIN_DISTINCT_CHARS: usize = 8;
/// Minimum accepted `BRIDGE_SECRET` length in bytes.
pub const MIN_SECRET_LEN: usize = 32;

/// Key material redacted from debug output and cleared when dropped.
pub struct SecretKey(Zeroizing<[u8; 32]>);

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Wrapper that hides a `String` value from `Debug` output.
pub struct Redacted(pub String);

impl std::fmt::Debug for Redacted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Read the configured `BRIDGE_SECRET`, rejecting set-but-weak values.
///
/// Returns `Ok(None)` when unset to preserve the existing optional plaintext
/// storage mode. A present but weak or non-Unicode value is an actionable error
/// and MUST NOT be treated as "encryption disabled".
pub fn read_bridge_secret() -> Result<Option<Zeroizing<String>>, IdentityError> {
    match std::env::var("BRIDGE_SECRET") {
        Ok(secret) => {
            validate_secret_strength(&secret)?;
            Ok(Some(Zeroizing::new(secret)))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(IdentityError::WeakBridgeSecret),
    }
}

/// Validate the currently configured `BRIDGE_SECRET`.
///
/// Returns `Ok(true)` when a usable secret is configured, `Ok(false)` when the
/// variable is unset, and [`IdentityError::WeakBridgeSecret`] with actionable
/// guidance for empty, short, or low-diversity values.
pub fn validate_bridge_secret() -> Result<bool, IdentityError> {
    read_bridge_secret().map(|secret| secret.is_some())
}

fn validate_secret_strength(secret: &str) -> Result<(), IdentityError> {
    if secret.len() < MIN_SECRET_LEN {
        return Err(IdentityError::WeakBridgeSecret);
    }
    let mut seen = [false; 256];
    let mut distinct = 0usize;
    for &byte in secret.as_bytes() {
        if !seen[byte as usize] {
            seen[byte as usize] = true;
            distinct += 1;
        }
    }
    if distinct < MIN_DISTINCT_CHARS {
        return Err(IdentityError::WeakBridgeSecret);
    }
    Ok(())
}

/// Validate a candidate secret without changing process environment.
pub fn validate_candidate_secret(secret: &str) -> Result<(), IdentityError> {
    validate_secret_strength(secret)
}

fn derive_key(password: &str, salt: &[u8; SALT_LEN]) -> Result<SecretKey, IdentityError> {
    let mut output = Zeroizing::new([0_u8; 32]);
    let params = argon2::Params::new(19 * 1024, 2, 1, Some(32))
        .map_err(|_| IdentityError::WeakBridgeSecret)?;
    Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, output.as_mut())
        .map_err(|_| IdentityError::WeakBridgeSecret)?;
    Ok(SecretKey(Zeroizing::new(*output)))
}

/// Encrypt `plaintext` with AES-256-GCM under the versioned GBCK envelope.
///
/// Each call uses a new random 16-byte salt and 12-byte nonce. Returns
/// `Ok(None)` if `BRIDGE_SECRET` is unset, allowing the caller to preserve
/// optional plaintext mode. Invalid configured secrets are returned as errors.
pub fn encrypt(plaintext: &[u8]) -> Result<Option<Vec<u8>>, IdentityError> {
    let Some(secret) = read_bridge_secret()? else {
        return Ok(None);
    };
    let mut salt = [0_u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let key = derive_key(&secret, &salt)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key.0[..]));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| IdentityError::Storage)?;

    let mut out = Vec::with_capacity(HEADER_LEN + SALT_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.push(FORMAT_ARGON2ID);
    out.extend_from_slice(&salt);
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ciphertext);
    Ok(Some(out))
}

/// Decrypt new GBCK envelopes or legacy nonce-prefixed SHA-256 ciphertext.
///
/// Authentication and parsing errors use the non-sensitive `Storage` variant;
/// neither the configured secret nor decrypted bytes enter error messages.
pub fn decrypt(data: &[u8]) -> Result<Vec<u8>, IdentityError> {
    let Some(secret) = read_bridge_secret()? else {
        return Err(IdentityError::Storage);
    };

    if data.starts_with(MAGIC) {
        if data.get(4) != Some(&FORMAT_ARGON2ID) {
            return Err(IdentityError::Storage);
        }
        return decrypt_envelope(data, &secret);
    }
    decrypt_legacy_sha256(data, &secret)
}

fn decrypt_envelope(data: &[u8], password: &str) -> Result<Vec<u8>, IdentityError> {
    let nonce_start = HEADER_LEN + SALT_LEN;
    let body_start = nonce_start + NONCE_LEN;
    if data.len() < body_start + 16 {
        return Err(IdentityError::Storage);
    }
    let salt: &[u8; SALT_LEN] = data[HEADER_LEN..nonce_start]
        .try_into()
        .map_err(|_| IdentityError::Storage)?;
    let nonce = Nonce::from_slice(&data[nonce_start..body_start]);
    let key = derive_key(password, salt)?;
    Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key.0[..]))
        .decrypt(nonce, &data[body_start..])
        .map_err(|_| IdentityError::Storage)
}

/// Legacy scheme: AES-256 = SHA-256(password), file = nonce || ciphertext+tag.
fn decrypt_legacy_sha256(data: &[u8], password: &str) -> Result<Vec<u8>, IdentityError> {
    if data.len() < NONCE_LEN + 16 {
        return Err(IdentityError::Storage);
    }
    let digest = Sha256::digest(password.as_bytes());
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&digest));
    let nonce = Nonce::from_slice(&data[..NONCE_LEN]);
    cipher
        .decrypt(nonce, &data[NONCE_LEN..])
        .map_err(|_| IdentityError::Storage)
}

/// Return true when an encrypted file uses the old nonce-prefixed format.
pub fn is_legacy_ciphertext(data: &[u8]) -> bool {
    !data.starts_with(MAGIC)
}

#[cfg(test)]
mod tests {
    use super::{SecretKey, derive_key};

    #[test]
    fn secret_key_debug_output_is_redacted() {
        let key: SecretKey =
            derive_key("correct-horse-battery-staple-bridge-A", b"0123456789abcdef").unwrap();
        assert_eq!(format!("{key:?}"), "<redacted>");
    }
}
