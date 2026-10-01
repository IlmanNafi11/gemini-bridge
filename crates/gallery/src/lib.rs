//! Gallery API service for listing, downloading and deleting local media.

mod handlers;

use bytes::Bytes;
use gemini_bridge_media_store::MediaMetadata;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use handlers::DefaultGalleryService;

const GALLERY_HTML: &str = include_str!("../static/gallery.html");

/// Query parameters for gallery listing.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GalleryQuery {
    /// Response format (`json` by default or `html`).
    pub format: Option<String>,
    /// Case-insensitive substring filter over prompt metadata.
    pub prompt: Option<String>,
    /// Exact model identifier filter.
    pub model: Option<String>,
    /// Inclusive lower bound on Unix creation timestamp.
    pub date_from: Option<i64>,
    /// Inclusive upper bound on Unix creation timestamp.
    pub date_to: Option<i64>,
    /// Maximum number of records, default 50 and maximum 200.
    pub limit: Option<usize>,
    /// Number of records to skip, default 0.
    pub offset: Option<usize>,
}

/// Public representation of one stored media record.
#[derive(Debug, Clone, Serialize)]
pub struct GalleryItem {
    pub id: String,
    pub url: String,
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub created_at: i64,
    pub mime_type: String,
    pub size_bytes: u64,
}

/// Paginated gallery response.
#[derive(Debug, Clone, Serialize)]
pub struct GalleryResponse {
    pub data: Vec<GalleryItem>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

/// Gallery service failures.
#[derive(Debug, Error)]
pub enum GalleryError {
    #[error("Invalid gallery query: {0}")]
    InvalidQuery(String),
    #[error("Gallery item not found: {0}")]
    NotFound(String),
    #[error("Media store error: {0}")]
    StoreError(String),
}

/// Operations on the local gallery.
#[async_trait::async_trait]
pub trait GalleryService: Send + Sync {
    /// List stored media matching filters, newest first, paginated.
    async fn list(&self, query: GalleryQuery) -> Result<GalleryResponse, GalleryError>;
    /// Retrieve item bytes and MIME metadata for download.
    async fn download(&self, id: &str) -> Result<(Bytes, MediaMetadata), GalleryError>;
    /// Delete a media record.
    async fn delete(&self, id: &str) -> Result<(), GalleryError>;
}

/// Static embedded HTML gallery page.
pub fn gallery_html() -> &'static str {
    GALLERY_HTML
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_html_has_gallery_controls_and_no_external_assets() {
        let html = gallery_html();
        assert!(html.contains("id=\"gallery\""));
        assert!(html.contains("id=\"btn-filter\""));
        assert!(html.contains("fetch('/gallery"));
        assert!(!html.contains("https://"));
        assert!(!html.contains("http://"));
    }
}
