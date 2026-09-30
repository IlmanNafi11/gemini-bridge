//! Content-addressed local media storage for gemini-bridge.
//!
//! Re-exports everything callers need from the public API.

pub mod cleanup;
mod error;
mod metadata;
mod store;

pub use error::StoreError;
pub use metadata::MediaMetadata;
pub use store::LocalMediaStore;

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
    /// * If the content file already exists on disk (same SHA-256) it is NOT
    ///   overwritten — the call is idempotent for the content bytes.
    async fn put(
        &self,
        content: bytes::Bytes,
        metadata: MediaMetadata,
    ) -> Result<String, StoreError>;

    /// Retrieve content bytes and metadata by `id`.
    async fn get(&self, id: &str) -> Result<(bytes::Bytes, MediaMetadata), StoreError>;

    /// Return `true` if a content file for `sha256` exists on disk.
    async fn exists(&self, sha256: &str) -> bool;

    /// Return the first metadata `id` whose `sha256` field matches `sha256`,
    /// or `None` if no such record exists.
    async fn find_by_hash(&self, sha256: &str) -> Option<String>;

    /// List metadata records sorted descending by `created_at` (newest first),
    /// with `id` as a stable tie-breaker.
    async fn list(&self, limit: usize, offset: usize) -> Vec<MediaMetadata>;

    /// Remove the metadata record for `id`. If no other metadata record
    /// references the same `sha256`, the content file is also removed.
    async fn delete(&self, id: &str) -> Result<(), StoreError>;

    /// Remove all metadata records whose `expires_at` is in the past, plus
    /// any unreferenced content files. Returns the number of records purged.
    async fn purge_expired(&self) -> Result<usize, StoreError>;
}
