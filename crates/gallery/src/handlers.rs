//! Gallery query processing and execution handlers.

use std::sync::Arc;

use bytes::Bytes;
use gemini_bridge_media_store::{MediaMetadata, MediaStore, MetadataFilter, StoreError};

use crate::{GalleryError, GalleryItem, GalleryQuery, GalleryResponse, GalleryService};

/// Default implementation of [`GalleryService`] backed by any [`MediaStore`].
pub struct DefaultGalleryService {
    store: Arc<dyn MediaStore>,
}

impl DefaultGalleryService {
    /// Create a new [`DefaultGalleryService`].
    pub fn new(store: Arc<dyn MediaStore>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl GalleryService for DefaultGalleryService {
    async fn list(&self, query: GalleryQuery) -> Result<GalleryResponse, GalleryError> {
        let limit = query.limit.unwrap_or(50);
        if limit == 0 || limit > 200 {
            return Err(GalleryError::InvalidQuery(
                "limit must be between 1 and 200".to_string(),
            ));
        }
        let offset = query.offset.unwrap_or(0);

        if let (Some(from), Some(to)) = (query.date_from, query.date_to)
            && from > to
        {
            return Err(GalleryError::InvalidQuery(
                "date_from must not be greater than date_to".to_string(),
            ));
        }

        let page = self
            .store
            .list_filtered(
                MetadataFilter {
                    prompt_contains: query.prompt,
                    model: query.model,
                    created_at_from: query.date_from,
                    created_at_to: query.date_to,
                },
                limit,
                offset,
            )
            .await
            .map_err(|error| GalleryError::StoreError(error.to_string()))?;

        let total = page.total;
        let data = page
            .items
            .into_iter()
            .map(|meta| GalleryItem {
                url: format!("/gallery/{}/download", meta.id),
                id: meta.id,
                prompt: meta.prompt,
                model: meta.model,
                created_at: meta.created_at,
                mime_type: meta.mime_type,
                size_bytes: meta.size_bytes,
            })
            .collect();

        Ok(GalleryResponse {
            data,
            total,
            limit,
            offset,
        })
    }

    async fn download(&self, id: &str) -> Result<(Bytes, MediaMetadata), GalleryError> {
        self.store.get(id).await.map_err(|e| match e {
            StoreError::NotFound(_) => GalleryError::NotFound(id.to_string()),
            other => GalleryError::StoreError(other.to_string()),
        })
    }

    async fn delete(&self, id: &str) -> Result<(), GalleryError> {
        self.store.delete(id).await.map_err(|e| match e {
            StoreError::NotFound(_) => GalleryError::NotFound(id.to_string()),
            other => GalleryError::StoreError(other.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gemini_bridge_media_store::LocalMediaStore;
    use tempfile::TempDir;

    fn make_meta(prompt: Option<&str>, model: Option<&str>, created_at: i64) -> MediaMetadata {
        MediaMetadata {
            id: String::new(),
            sha256: String::new(),
            mime_type: "image/png".to_string(),
            size_bytes: 4,
            created_at,
            expires_at: None,
            prompt: prompt.map(String::from),
            file_ref: String::new(),
            model: model.map(String::from),
        }
    }

    #[tokio::test]
    async fn test_empty_list() {
        let temp = TempDir::new().unwrap();
        let store = Arc::new(LocalMediaStore::new(temp.path()));
        let service = DefaultGalleryService::new(store);

        let res = service.list(GalleryQuery::default()).await.unwrap();
        assert_eq!(res.total, 0);
        assert!(res.data.is_empty());
    }

    #[tokio::test]
    async fn test_filter_and_pagination() {
        let temp = TempDir::new().unwrap();
        let store = Arc::new(LocalMediaStore::new(temp.path()));
        let service = DefaultGalleryService::new(store.clone());

        store
            .put(
                Bytes::from_static(b"img1"),
                make_meta(Some("Apple pie"), Some("flash"), 100),
            )
            .await
            .unwrap();
        store
            .put(
                Bytes::from_static(b"img2"),
                make_meta(Some("Banana split"), Some("pro"), 200),
            )
            .await
            .unwrap();
        store
            .put(
                Bytes::from_static(b"img3"),
                make_meta(Some("Apple cider"), Some("flash"), 300),
            )
            .await
            .unwrap();

        // Prompt filter: "apple" (case-insensitive)
        let res = service
            .list(GalleryQuery {
                prompt: Some("apple".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(res.total, 2);
        assert_eq!(res.data.len(), 2);
        assert_eq!(res.data[0].created_at, 300); // newest first

        // Model filter: "pro"
        let res = service
            .list(GalleryQuery {
                model: Some("pro".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(res.total, 1);
        assert_eq!(res.data[0].prompt.as_deref(), Some("Banana split"));

        // Date range
        let res = service
            .list(GalleryQuery {
                date_from: Some(150),
                date_to: Some(250),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(res.total, 1);
        assert_eq!(res.data[0].created_at, 200);
    }
}
