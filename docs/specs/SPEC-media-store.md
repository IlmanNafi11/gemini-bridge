# Module Specification: `media-store`

**Module ID:** `media-store`  
**Crate:** `gemini-bridge-media-store` (`crates/media-store`)  
**Phase:** Fase 1  
**Depends On:** `config`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-2, US-3, US-6, §1.3  
**Status:** Approved Draft — enriched for P.5  

---

## 1. Objective & Responsibility

The `media-store` module provides content-addressed local filesystem storage for generated images and uploaded media files, using SHA-256 hashes as the content identity key. It decouples media payload storage from SQLite metadata persistence, manages per-item metadata (prompt, model, MIME type, creation timestamp, expiry timestamp), guarantees deduplication, handles safe concurrent writes, marks records with TTL-based expiry, and provides an explicit administrative purge primitive. Expiry does not trigger scheduled or automatic deletion.

**In scope:**
- Content-addressed storage on the local filesystem under `{data_dir}/media/{sha256[0..2]}/{sha256}`.
- JSON metadata sidecars stored under `{data_dir}/media/meta/{id}.json`, with a durable SQLite metadata index for bounded filtered queries and rebuild/recovery.
- Idempotent content writing (content bytes with identical SHA-256 are never duplicated or overwritten), including stable-ID replacement with reference-safe cleanup.
- Metadata CRUD operations (`put`, `get`, `exists`, `find_by_hash`, `list`, bounded `list_filtered`, `delete`).
- Pagination and sorting of stored media records (newest first), capped at 200 rows per operation.
- Expiration calculation and explicit admin-triggered TTL purge (`purge_expired`), removing only expired metadata and unreferenced content files; no scheduled cleanup is performed by this module.
- Strict path traversal prevention on all input identifiers and hashes.

**Out of scope:**
- Resumable push upload to Google or network I/O (→ `upload`).
- Image generation prompt orchestration or URL extraction (→ `image-gen`).
- HTTP routing, SSE streaming, or endpoint authentication (→ `http-server`, `gallery`).
- Conversation history or relational branching (→ `conversation-store`).

---

## 2. Public API & Interfaces

### 2.1 Metadata Record

```rust
use serde::{Deserialize, Serialize};

/// Metadata record for a stored media object.
///
/// `id` is a stable opaque identifier (UUID v4 by default). `sha256` is the
/// hex-encoded SHA-256 of the raw bytes — it is both the content identity and
/// the on-disk file name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaMetadata {
    /// Stable opaque identifier; generated if empty on `put`.
    pub id: String,
    /// Hex-encoded SHA-256 of the content bytes; computed and filled by `put`.
    pub sha256: String,
    /// MIME type of the content (e.g. `"image/png"`, `"image/jpeg"`).
    pub mime_type: String,
    /// Size of the content in bytes; set by `put`.
    pub size_bytes: u64,
    /// Unix timestamp (seconds) when the record was created.
    pub created_at: i64,
    /// Optional Unix timestamp (seconds) after which the record is marked expired
    /// and eligible for an explicit administrative purge. `None` means the record never expires.
    pub expires_at: Option<i64>,
    /// Optional generation prompt (for image/video records).
    pub prompt: Option<String>,
    /// Optional Gemini fileRef acquired from the upstream push-upload flow.
    /// Empty for media that has not been uploaded as an upstream reference.
    #[serde(default)]
    pub file_ref: String,
    /// Optional model identifier used to generate the media.
    pub model: Option<String>,
}
```

### 2.2 Errors

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("Object not found: {0}")]
    NotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Content hash mismatch")]
    HashMismatch,

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
}
```

### 2.3 Store Trait and Local Implementation

```rust
use bytes::Bytes;
use std::path::PathBuf;
use std::sync::Arc;

/// Maximum number of metadata rows returned by one list operation.
pub const MAX_LIST_PAGE_SIZE: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataFilter {
    pub prompt_contains: Option<String>,
    pub model: Option<String>,
    pub created_at_from: Option<i64>,
    pub created_at_to: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataPage {
    pub items: Vec<MediaMetadata>,
    pub total: usize,
}

/// The public `MediaStore` trait. All callers and tests work against this
/// interface — not against the concrete implementation.
#[async_trait::async_trait]
pub trait MediaStore: Send + Sync {
    /// Store `content` and associated `metadata`. Returns the assigned `id`.
    ///
    /// # Behavior
    /// * If `metadata.sha256` is non-empty and does not match the actual
    ///   SHA-256 of `content`, the call fails with [`StoreError::HashMismatch`].
    /// * If `metadata.sha256` is empty it is computed and filled in.
    /// * If `metadata.id` is empty a fresh UUID is generated.
    /// * If `metadata.id` names an existing record, its metadata is replaced and
    ///   no-longer-referenced prior content is removed.
    /// * If the content file already exists on disk (same SHA-256) it is NOT
    ///   overwritten — the call is idempotent for the content bytes.
    async fn put(
        &self,
        content: Bytes,
        metadata: MediaMetadata,
    ) -> Result<String, StoreError>;

    /// Retrieve content bytes and metadata by `id`.
    async fn get(&self, id: &str) -> Result<(Bytes, MediaMetadata), StoreError>;

    /// Return `true` if a content file for `sha256` exists on disk.
    async fn exists(&self, sha256: &str) -> bool;

    /// Return the first metadata `id` whose `sha256` field matches `sha256`,
    /// or `None` if no such record exists.
    async fn find_by_hash(&self, sha256: &str) -> Option<String>;

    /// List metadata records sorted descending by `created_at` (newest first),
    /// with `id` as a stable tie-breaker. At most 200 records are returned,
    /// even if `limit` is larger.
    async fn list(&self, limit: usize, offset: usize) -> Vec<MediaMetadata>;

    /// Filter and paginate through the durable index without scanning every
    /// sidecar. The result contains a bounded page and matching-row total.
    async fn list_filtered(
        &self,
        filter: MetadataFilter,
        limit: usize,
        offset: usize,
    ) -> Result<MetadataPage, StoreError>;

    /// Remove the metadata record for `id`. If no other metadata record
    /// references the same `sha256`, the content file is also removed.
    async fn delete(&self, id: &str) -> Result<(), StoreError>;

    /// Explicitly remove metadata records whose `expires_at` is in the past, plus
    /// any unreferenced content files. Nothing invokes this on a schedule;
    /// returns the number of records purged.
    async fn purge_expired(&self) -> Result<usize, StoreError>;
}

/// Filesystem-backed content-addressed media store, with durable SQLite index.
#[derive(Clone)]
pub struct LocalMediaStore {
    data_dir: PathBuf,
    default_ttl_days: u32,
    index: Arc<MetadataIndex>,
}

impl LocalMediaStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self;
    /// Apply a default expiry to records that do not already specify one.
    pub fn with_default_ttl_days(self, ttl_days: u32) -> Self;
}
```

### 2.4 Cleanup and TTL Utilities

```rust
pub mod cleanup {
    use super::MediaMetadata;

    /// Return `true` if `metadata` has an `expires_at` timestamp strictly less than `now_unix`.
    pub fn is_expired(metadata: &MediaMetadata, now_unix: i64) -> bool;

    /// Current Unix timestamp in seconds.
    pub fn now_unix() -> i64;

    /// Compute default expiry timestamp from `created_at` + `ttl_days` (86400s per day).
    /// Expiry marks the record as eligible for manual purge; it does not delete it.
    /// Returns `None` if `ttl_days == 0`.
    pub fn compute_default_expiry(created_at: i64, ttl_days: u32) -> Option<i64>;
}
```

---

## 3. Behavior & Invariants

1. **Content Addressing & Sharding:**
   Files are stored at `{data_dir}/media/{sha256[0..2]}/{sha256}`. The two-character hex prefix shards files across 256 subdirectories to prevent filesystem performance degradation from flat directory saturation.

2. **Durable Index and Sidecar Recovery:**
   SQLite indexes metadata and supports bounded, filtered/paginated queries. Metadata sidecars remain the durable record source; the index is rebuilt/recovered from sidecars when its persisted state indicates an incomplete mutation or invalid index data.

3. **Atomic Metadata Persistence:**
   Metadata records are serialized to JSON and written via per-write temporary files (`{data_dir}/media/meta/.{id}.{uuid}.tmp`) followed by atomic filesystem rename (`tokio::fs::rename`).

4. **Content Idempotency & Stable-ID Replacement:**
   `put` with content matching an existing SHA-256 skips rewriting content bytes. Multiple metadata records may reference the same SHA-256. Replacing a record under an existing stable `id` releases old content only if no other metadata record references it.

5. **Reference-Counted Deletion:**
   When `delete(id)` or an explicit `purge_expired()` call removes a metadata record, it checks whether any remaining metadata record references the same `sha256`. The content file is removed from disk **only** when reference count reaches zero.

6. **Expiry and Purge Safety:**
   `expires_at` marks when an item becomes eligible for purge; it is not an automatic deletion timer. `purge_expired()` evaluates `expires_at < now_unix()`. Items with `expires_at == None` or `expires_at >= now_unix()` are preserved. Purging occurs only when the administrator explicitly invokes the protected purge operation.

7. **Private Storage and Concurrency:**
   On Unix, store directories are mode `0700`, and content, metadata, lock, and SQLite files are mode `0600`. Mutations use a filesystem lock to serialize across cloned stores, independent handles, and processes; interrupted indexed mutations are recovered through the durable sidecar/index recovery path.

---

## 4. Acceptance Criteria

### US-2 / US-3 / US-6 Traceability

| PRD Requirement | Feature | Covered by `media-store` |
|---|---|---|
| US-2 (Image Generation) | Local caching of generated images with metadata (prompt, model, time) | `put` with `prompt`, `model`, `mime_type`, SHA-256 content addressing |
| US-2 (Image Retrieval) | Retrieve generated image by ID | `get(id)` returning content bytes + `MediaMetadata` |
| US-3 (Multimodal Upload) | Local caching of uploaded reference files | `put` content bytes; deduplication by SHA-256 |
| US-3 (Deduplication) | Reuse fileRef based on content hash | `find_by_hash(sha256)` / `exists(sha256)` |
| US-6 (Gallery & Media Mgmt) | List stored media with pagination & sort | `list(limit, offset)` sorted descending by `created_at` |
| US-6 (Gallery Operations) | Delete stored media | `delete(id)` with unreferenced content cleanup |
| US-6 (TTL Expiry) | Mark records expired after configured TTL; remove them only on admin request | `compute_default_expiry` records eligibility; protected `/admin/purge` invokes `purge_expired()` manually |

### Module-Level Acceptance Criteria

1. `put` computes SHA-256, assigns UUID v4 if absent, writes sharded content bytes and JSON metadata sidecar, and returns valid `id`.
2. `put` with mismatched `metadata.sha256` returns `StoreError::HashMismatch` without writing files.
3. Storing identical content bytes twice creates two metadata records pointing to the same single content file on disk.
4. `get(id)` returns the exact stored bytes and complete metadata struct; returns `StoreError::NotFound` for unknown or malformed IDs.
5. `list(limit, offset)` and `list_filtered(...)` use the durable index, sort by `created_at` then stable `id`, honor pagination, and cap a page at 200 records.
6. Reopening after index deletion/corruption or an interrupted mutation rebuilds a consistent index from metadata sidecars.
7. Replacing metadata under a stable `id` removes the old content only when no other record references it.
8. `delete(id)` deletes the metadata record; deletes the sharded content file if and only if no other metadata record references that SHA-256.
9. An explicit `purge_expired()` call deletes records where `expires_at < now_unix()`, cleans up unreferenced content files, and returns the exact count of purged items. Expired records remain stored until that call is made.
10. Store paths/files use mode `0700`/`0600` on Unix, and concurrent independent store handles/processes serialize mutations through the filesystem lock.
11. IDs containing path traversal patterns (`../`, `..\\`) are rejected without error leaks or filesystem escapes.

---

## 5. Testing Strategy

- **Unit & Integration Tests (`crates/media-store/tests/store_test.rs`):**
  - `put_and_get_round_trip`: Store bytes and metadata, retrieve by ID, verify byte equality and field match.
  - `put_fills_sha256_and_size`: Validate auto-computed hash and size fields.
  - `put_rejects_wrong_sha256`: Verify `StoreError::HashMismatch` rejection.
  - `deduplication_same_sha256_one_content_file`: Store two identical payloads; assert single file under `{data_dir}/media/{prefix}/`.
  - `exists_and_find_by_hash`: Assert hash discovery behavior before and after `put`.
  - `list_respects_limit_and_offset`: Test empty list, full list, and offset/limit pagination.
  - `delete_reference_counting`: Test delete with single reference (content removed) vs. dual reference (content retained until second delete).
  - `purge_expired_behavior`: Verify past-due items purged, active items retained, boundary timestamp (`expires_at == now`) preserved.
  - `path_traversal_rejection`: Test `../escape` and `..\\escape` IDs return `NotFound`.
  - `list_filtered_uses_stable_bounded_pagination`: Verify filters, deterministic tie ordering, matching-row totals, and the 200-row cap.
  - `index_rebuild_and_interrupted_mutation_recovery`: Delete/corrupt the index or leave recovery state and verify sidecar-driven recovery.
  - `stable_id_replacement_is_reference_safe`: Replace a record's content and preserve any old content still referenced elsewhere.
  - `cross_handle_and_process_mutations_are_serialized`: Exercise independently constructed handles and subprocess writers.
  - Unix permission tests assert `0700` directories and `0600` content, metadata, lock, and SQLite files.

---

## 6. Boundaries

- **Always:** Shard content files under `{data_dir}/media/{sha256[0..2]}/{sha256}`; compute SHA-256 over raw bytes; write metadata atomically; guard against path traversal; serialize mutations; describe expiry as a purge eligibility marker, not scheduled deletion.
- **Ask First:** Changing directory layout, metadata JSON schema, or default TTL period.
- **Never:** Overwrite existing content bytes for a given SHA-256; delete content files that are still referenced by other metadata; log or store session tokens/credentials in media metadata; panic on IO errors.
