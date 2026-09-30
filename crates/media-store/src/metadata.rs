use serde::{Deserialize, Serialize};

/// Metadata record for a stored media object.
///
/// `id` is a stable opaque identifier (UUID v4 by default).  `sha256` is the
/// hex-encoded SHA-256 of the raw bytes — it is both the content identity and
/// the on-disk file name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaMetadata {
    /// Stable opaque identifier; generated if empty on `put`.
    pub id: String,
    /// Hex-encoded SHA-256 of the content bytes; computed and filled by `put`.
    pub sha256: String,
    /// MIME type of the content (e.g. `"image/png"`).
    pub mime_type: String,
    /// Size of the content in bytes; set by `put`.
    pub size_bytes: u64,
    /// Unix timestamp (seconds) when the record was created.
    pub created_at: i64,
    /// Optional Unix timestamp (seconds) after which the record is eligible
    /// for purge. `None` means the record never expires.
    pub expires_at: Option<i64>,
    /// Optional generation prompt (for image/video records).
    pub prompt: Option<String>,
    /// Optional model identifier used to generate the media.
    pub model: Option<String>,
}
