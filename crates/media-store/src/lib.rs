//! Content-addressed local media storage for gemini-bridge.
//!
//! Re-exports everything callers need from the public API.

pub mod cleanup;
mod error;
mod index;
mod metadata;
mod store;

pub use error::StoreError;
pub use metadata::MediaMetadata;
pub use store::LocalMediaStore;

/// Maximum number of metadata rows returned by one list operation.
pub const MAX_LIST_PAGE_SIZE: usize = 200;

/// Optional metadata constraints evaluated by the durable store index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataFilter {
    /// Case-insensitive substring matched against `prompt`.
    pub prompt_contains: Option<String>,
    /// Exact model identifier.
    pub model: Option<String>,
    /// Inclusive lower bound for `created_at`.
    pub created_at_from: Option<i64>,
    /// Inclusive upper bound for `created_at`.
    pub created_at_to: Option<i64>,
}

/// One bounded page from the metadata index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataPage {
    pub items: Vec<MediaMetadata>,
    /// Number of all records matching the filter before pagination.
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
    ///   any no-longer-referenced prior content is removed.
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
    /// with `id` as a stable tie-breaker. At most [`MAX_LIST_PAGE_SIZE`]
    /// records are returned, even if `limit` is larger.
    async fn list(&self, limit: usize, offset: usize) -> Vec<MediaMetadata>;

    /// Filter and paginate metadata in the durable index without reading every
    /// sidecar. At most [`MAX_LIST_PAGE_SIZE`] records are returned.
    async fn list_filtered(
        &self,
        filter: MetadataFilter,
        limit: usize,
        offset: usize,
    ) -> Result<MetadataPage, StoreError> {
        let mut matching: Vec<_> = self
            .list(usize::MAX, 0)
            .await
            .into_iter()
            .filter(|metadata| {
                filter
                    .model
                    .as_ref()
                    .is_none_or(|model| metadata.model.as_ref() == Some(model))
                    && filter
                        .created_at_from
                        .is_none_or(|from| metadata.created_at >= from)
                    && filter
                        .created_at_to
                        .is_none_or(|to| metadata.created_at <= to)
                    && filter.prompt_contains.as_ref().is_none_or(|needle| {
                        metadata
                            .prompt
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&needle.to_lowercase())
                    })
            })
            .collect();
        matching.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let total = matching.len();
        let items = matching
            .into_iter()
            .skip(offset)
            .take(limit.min(MAX_LIST_PAGE_SIZE))
            .collect();
        Ok(MetadataPage { items, total })
    }

    /// Remove the metadata record for `id`. If no other metadata record
    /// references the same `sha256`, the content file is also removed.
    async fn delete(&self, id: &str) -> Result<(), StoreError>;

    /// Remove all metadata records whose `expires_at` is in the past, plus
    /// any unreferenced content files. Returns the number of records purged.
    async fn purge_expired(&self) -> Result<usize, StoreError>;
}
