# Module Specification: `media-store`

**Module ID:** `media-store`  
**Crate:** `gemini-bridge-media-store` (`crates/media-store`)  
**Phase:** Fase 1  
**Depends On:** `config`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-2, US-3  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Provide content-addressed local file storage for images and uploaded media using SHA-256 as the identity key. Handle metadata persistence (prompt, model, creation time, MIME, dimensions), TTL-based expiry, and a purge/cleanup API.

---

## 2. Public API & Interfaces

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("Object not found: {0}")]
    NotFound(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Content hash mismatch")]
    HashMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaMetadata {
    pub id: String,
    pub sha256: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub prompt: Option<String>,
    pub model: Option<String>,
}

#[async_trait::async_trait]
pub trait MediaStore: Send + Sync {
    async fn put(&self, content: bytes::Bytes, metadata: MediaMetadata) -> Result<String, StoreError>;
    async fn get(&self, id: &str) -> Result<(bytes::Bytes, MediaMetadata), StoreError>;
    async fn exists(&self, sha256: &str) -> bool;
    async fn find_by_hash(&self, sha256: &str) -> Option<String>;
    async fn list(&self, limit: usize, offset: usize) -> Vec<MediaMetadata>;
    async fn delete(&self, id: &str) -> Result<(), StoreError>;
    async fn purge_expired(&self) -> Result<usize, StoreError>;
}
```

---

## 3. Behavior & Invariants

1. **Content Identity:** Files are stored at `{data_dir}/media/{sha256[0..2]}/{sha256}`, ensuring deduplication by hash.
2. **Metadata Separation:** Metadata stored alongside or in a companion file; no SQLite dependency in this crate.
3. **No Double-Write:** `put` with a matching hash must be idempotent and not overwrite.
4. **Purge Safety:** `purge_expired` removes only items whose `expires_at` has passed; active/unexpired items are never affected.

---

## 4. Testing Strategy

- `put`/`get` round-trip with hash verification.
- Deduplication: two `put` calls with identical content yield the same ID and one on-disk file.
- `purge_expired` removes only expired items, not fresh ones.

---

## 5. Boundaries

- **Always:** Preserve content exactly; compute SHA-256 on received bytes before storing.
- **Ask First:** Changing directory layout or metadata serialization format.
- **Never:** Overwrite existing stored content; store credentials or session tokens.
