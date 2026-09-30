use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use sha2::{Digest, Sha256};

/// Key material redacted from debug output.
pub struct SecretKey(Key<Aes256Gcm>);

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Derive a 32-byte AES-256 key from the `BRIDGE_SECRET` env variable.
///
/// Returns `None` if `BRIDGE_SECRET` is not set or is empty.
pub fn derive_key() -> Option<SecretKey> {
    let secret = std::env::var("BRIDGE_SECRET").ok()?;
    if secret.is_empty() {
        return None;
    }
    let hash = Sha256::digest(secret.as_bytes());
    Some(SecretKey(*Key::<Aes256Gcm>::from_slice(&hash)))
}

/// Encrypt `plaintext` with AES-256-GCM using a random 12-byte nonce.
///
/// The returned bytes are `nonce (12 bytes) || ciphertext+tag`.
///
/// Returns `None` if `BRIDGE_SECRET` is not configured.
pub fn encrypt(plaintext: &[u8]) -> Option<Vec<u8>> {
    let key = derive_key()?;
    let cipher = Aes256Gcm::new(&key.0);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher.encrypt(&nonce, plaintext).ok()?;
    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ciphertext);
    Some(out)
}

/// Decrypt bytes produced by [`encrypt`].
///
/// The first 12 bytes are the nonce; the remainder is ciphertext+tag.
///
/// Returns `None` if the key is unavailable, the slice is too short, or
/// authentication fails (wrong key / tampered data).
pub fn decrypt(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 12 {
        return None;
    }
    let key = derive_key()?;
    let cipher = Aes256Gcm::new(&key.0);
    let nonce = Nonce::from_slice(&data[..12]);
    cipher.decrypt(nonce, &data[12..]).ok()
}

/// Wrapper that hides a `String` value from `Debug` output.
pub struct Redacted(pub String);

impl std::fmt::Debug for Redacted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}
