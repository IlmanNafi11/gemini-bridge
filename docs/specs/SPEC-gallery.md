# Module Specification: `gallery`

**Module ID:** `gallery`  
**Crate:** `gemini-bridge-gallery` (`crates/gallery`)  
**Phase:** Fase 2  
**Depends On:** `media-store`, `http-server`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-6  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Provide a media management surface: JSON listing of stored images with metadata, a lightweight single-file HTML UI embedded in the binary via `include_str!`, per-item download, and deletion. Supports filter parameters (date range, model, prompt substring) and paginated queries.

---

## 2. Public API & Interfaces

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/gallery` | JSON listing with `?format=html` for embedded HTML UI |
| `GET` | `/gallery?format=html` | Serve embedded static HTML |
| `DELETE` | `/gallery/{id}` | Delete item from store |

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct GalleryQuery {
    pub format: Option<String>,
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub date_from: Option<i64>,
    pub date_to: Option<i64>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct GalleryItem {
    pub id: String,
    pub url: String,
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub created_at: i64,
    pub mime_type: String,
    pub size_bytes: u64,
}
```

---

## 3. Behavior & Invariants

1. **Zero External Dependencies:** The embedded HTML/JS UI has no CDN links, external fonts, or dynamic API calls to third-party services. It must work entirely from the locally served binary.
2. **JSON Default:** When `format=html` is not set, response is `application/json` listing with metadata.
3. **Delete Safety:** Deletion removes from media-store; the ID cannot be re-used to serve the previous content.
4. **Filtering:** Query parameters are applied server-side; empty results are a 200 with an empty list, not a 404.

---

## 4. Testing Strategy

- JSON listing test with populated and empty media store.
- Filter correctness tests for each supported parameter.
- Deletion test verifying subsequent GET returns 404.
- HTML response content test asserting no external resource URLs in `<link>`, `<script>`, `<img>` tags.

---

## 5. Boundaries

- **Always:** Serve embedded HTML only with no external asset links; validate and sanitize filter parameters.
- **Ask First:** Adding non-JSON response formats or new filter dimensions.
- **Never:** Expose media store internals (paths, SHA-256 keys) in the public API; allow unauthenticated deletion in non-local binding.
