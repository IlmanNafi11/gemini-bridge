# Module Specification: `gallery`

**Module ID:** `gallery`  
**Crate:** `gemini-bridge-gallery` (`crates/gallery`)  
**Phase:** Fase 2  
**Depends On:** `media-store`, `http-server`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-6, §4.5, §4.7  
**Status:** Approved Draft — enriched for P.6  

---

## 1. Objective & Responsibility

The `gallery` module provides a lightweight local media management surface for generated images stored by `media-store`. It exposes a JSON listing endpoint and a small embedded HTML page, with server-side filters and pagination, plus per-item download and deletion operations.

**In scope:**
- `GET /gallery` — JSON list of stored gallery media, newest first.
- `GET /gallery?format=html` — return one static HTML page embedded in the binary via `include_str!`.
- `DELETE /gallery/{id}` — remove an item from the gallery through `MediaStore::delete`.
- Query filters for creation date range, model identifier, and prompt substring.
- Pagination with validated `limit` and `offset` parameters.
- Per-item download through the media store's retrieval API, with media type and content length headers.
- Minimal HTML/JS UI rendering thumbnails, prompt, model, timestamp, pagination and delete/download affordances.

**Out of scope:**
- Image generation, metadata creation, image extraction, or cache writes (→ `image-gen`, `media-store`).
- Thumbnail generation or image transformation (→ `media-store` or `image-gen` if later approved; current implementation returns stored image bytes as-is).
- HTTP server startup, route registration, bearer authentication, request IDs (→ `http-server`).
- Media TTL policy or scheduled cleanup (→ `media-store`, `health-admin`).
- External user accounts or remote gallery hosting.

---

## 2. Public API & Interfaces

### 2.1 Query and Response Models

```rust
use serde::{Deserialize, Serialize};

/// Query parameters for `GET /gallery` JSON listing.
#[derive(Debug, Clone, Deserialize)]
pub struct GalleryQuery {
    /// Response format. `None` or "json" returns JSON; "html" returns embedded page.
    pub format: Option<String>,
    /// Case-insensitive substring filter over media prompt metadata.
    pub prompt: Option<String>,
    /// Exact model identifier filter.
    pub model: Option<String>,
    /// Inclusive lower bound on Unix creation timestamp (seconds).
    pub date_from: Option<i64>,
    /// Inclusive upper bound on Unix creation timestamp (seconds).
    pub date_to: Option<i64>,
    /// Maximum number of records (default 50, maximum 200).
    pub limit: Option<usize>,
    /// Number of records to skip (default 0).
    pub offset: Option<usize>,
}

/// Public gallery representation of one stored media record.
#[derive(Debug, Clone, Serialize)]
pub struct GalleryItem {
    /// Opaque local identifier used by gallery item endpoints.
    pub id: String,
    /// Stable local path for retrieval, e.g. `/gallery/{id}/download`.
    pub url: String,
    /// Generation prompt when available.
    pub prompt: Option<String>,
    /// Model identifier when available.
    pub model: Option<String>,
    /// Unix creation timestamp (seconds).
    pub created_at: i64,
    /// MIME type (e.g. "image/png").
    pub mime_type: String,
    /// Content size in bytes.
    pub size_bytes: u64,
}

/// Paginated listing response.
#[derive(Debug, Clone, Serialize)]
pub struct GalleryResponse {
    /// Filtered gallery items for the requested page.
    pub data: Vec<GalleryItem>,
    /// Total records matching filters before pagination.
    pub total: usize,
    /// Maximum items requested for one page.
    pub limit: usize,
    /// Records skipped before this page.
    pub offset: usize,
}
```

### 2.2 Errors

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GalleryError {
    #[error("Invalid gallery query: {0}")]
    InvalidQuery(String),

    #[error("Gallery item not found: {0}")]
    NotFound(String),

    #[error("Media store error: {0}")]
    StoreError(String),
}
```

### 2.3 Service Trait

```rust
use bytes::Bytes;
use gemini_bridge_media_store::{MediaMetadata, MediaStore};

#[async_trait::async_trait]
pub trait GalleryService: Send + Sync {
    /// List media matching `query` filters, sorted newest first, paginated.
    async fn list(&self, query: GalleryQuery) -> Result<GalleryResponse, GalleryError>;

    /// Retrieve item bytes and MIME type for download.
    async fn download(&self, id: &str) -> Result<(Bytes, MediaMetadata), GalleryError>;

    /// Delete an item by local media-store ID.
    async fn delete(&self, id: &str) -> Result<(), GalleryError>;
}
```

---

## 3. Route Contract

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/gallery` | Optional API key per `http-server` policy | JSON listing by default; `?format=html` serves embedded page |
| `GET` | `/gallery/{id}/download` | Optional API key per `http-server` policy | Return original stored image bytes with detected MIME type |
| `DELETE` | `/gallery/{id}` | API key required unless binding is localhost-only | Delete gallery item and media metadata/content when unreferenced |

The `/gallery/{id}/download` route is the download action linked from the listing; it returns the stored bytes unchanged. `http-server` owns route wiring and applies the configured authentication policy. On non-local bind, listing/download follow normal API-key enforcement and deletion always requires a configured valid key.

---

## 4. Behavior & Invariants

1. **Self-Contained HTML Page:**
   Embedded HTML/JS has no CDN links, external fonts, external images, or third-party runtime assets. All gallery data requests go only to same-origin `/gallery` endpoints. The page remains functional with only the running bridge binary and does not require a build-time frontend toolchain.
2. **JSON Default:**
   `GET /gallery` without `format=html` returns `application/json`. Empty stores or filters with no matches return HTTP 200 and an empty `data` array with `total: 0`.
3. **Supported Filters:**
   - `date_from`: include items with `created_at >= date_from`.
   - `date_to`: include items with `created_at <= date_to`.
   - `model`: exact match.
   - `prompt`: case-insensitive substring match.
   All filters apply before pagination. If `date_from > date_to`, return `GalleryError::InvalidQuery` mapped to HTTP 400.
4. **Stable Pagination and Ordering:**
   Results sort descending by `created_at`, with opaque item `id` as deterministic tie-breaker. Default `limit` is 50; maximum is 200. Invalid/overflow limits are rejected with HTTP 400; offset defaults to zero.
5. **Opaque IDs and Safe Retrieval:**
   Public API returns only local opaque media IDs and stable bridge URLs. It never exposes filesystem paths, raw content hashes, or Gemini-issued expiring URLs. Download returns the stored bytes only for the requested existing ID; missing items map to 404.
6. **Deletion Semantics:**
   `DELETE /gallery/{id}` delegates to `MediaStore::delete`. If the content object is referenced by other metadata records, media-store retains the bytes; the deleted ID no longer resolves. Deleting an unknown ID returns 404.
7. **No Cross-Origin Calls:**
   The embedded page uses same-origin endpoints only. User-controlled prompt/model filter values are URL-encoded when inserted into requests and are rendered as text, not interpreted as HTML.

---

## 5. Acceptance Criteria

### US-6 Traceability (Gallery & Media Management)

| US-6 Acceptance Criterion | Covered by |
|---|---|
| List stored images and filter by date/model/prompt | `GET /gallery` JSON route; server-side `GalleryQuery` filtering |
| Delete and download images | `DELETE /gallery/{id}`; `GET /gallery/{id}/download` |
| `GET /gallery` returns JSON; `GET /gallery?format=html` returns lightweight view | JSON default plus embedded static HTML response |
| UI may be a single static page, no SPA requirement | `include_str!` page contract; no frontend build system required |

---

## 6. Testing Strategy

1. **JSON Listing Tests:**
   - Empty store returns HTTP 200 with `data: []` and `total: 0`.
   - Populated store returns records newest-first with stable ID tie-break ordering for equal timestamps.
2. **Filter Tests:**
   - `date_from`/`date_to` boundaries are inclusive.
   - `model` matches exactly and excludes other model IDs.
   - `prompt` matches case-insensitively as a substring.
   - Combined filters are applied before pagination; verify total matches count is independent of current page size.
3. **Pagination Boundary Tests:**
   - Default limit/offset returns first page in deterministic order.
   - `limit > 200`, `date_from > date_to`, and invalid formats return HTTP 400.
4. **Download and Delete Tests:**
   - Download returns exact original bytes and MIME type for a stored item.
   - Unknown item download returns 404.
   - Delete item then request same ID: subsequent download returns 404.
   - Deleting one metadata record does not remove shared bytes still referenced by another record (delegated invariant of `media-store`).
5. **HTML Isolation and Rendering Safety Tests:**
   - `GET /gallery?format=html` returns `text/html` with expected gallery controls.
   - Parse HTML references and assert no `src`/`href` resource points to an external host or scheme.
   - Prompt strings containing markup (e.g. `<script>`) appear escaped/rendered as text, not executable elements.
   - Static page's fetch targets are same-origin `/gallery` routes.
6. **Route Auth Tests:**
   - Verify gallery JSON and download routes follow server API key policy.
   - Verify delete route is protected when binding is not localhost-only.

---

## 7. Boundaries

- **Always:** Serve HTML from an embedded asset with no external runtime dependency; apply filters on the server; preserve newest-first stable ordering; return opaque local IDs and same-origin URLs; treat prompt/model input as untrusted text.
- **Ask First:** Adding non-JSON API response formats, new filter dimensions, pagination semantics changes, thumbnail generation, or remote gallery hosting.
- **Never:** Return filesystem paths, raw SHA-256 keys, or Gemini media URLs; perform external-origin fetches from the HTML UI; render user prompt/model text as trusted HTML; allow deletion on non-local binds without API-key protection.

---

## 8. Implementation Reference

- **Media storage contract:** `docs/specs/SPEC-media-store.md` (`MediaStore::list`, `get`, `delete`).
- **HTTP route and auth policy:** `docs/specs/SPEC-http-server.md` route registry and middleware contract.
- **Image generation metadata source:** `docs/specs/SPEC-image-gen.md` stores prompt/model metadata with generated images.
- **Future TTL/Purge administration:** Task 2.5 adds `POST /admin/purge` and media TTL controls; gallery itself does not own cleanup scheduling.
