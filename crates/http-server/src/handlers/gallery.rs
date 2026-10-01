//! Gallery HTTP route handlers.

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::AppState;
use gemini_bridge_gallery::{GalleryError, GalleryQuery, gallery_html};

/// `GET /gallery` — returns JSON listing by default or `?format=html` for the UI page.
pub async fn list_gallery(
    State(state): State<AppState>,
    Query(query): Query<GalleryQuery>,
) -> Response {
    let Some(service) = &state.gallery_service else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "gallery not available"})),
        )
            .into_response();
    };

    if let Some(format) = query.format.as_deref()
        && format != "html"
        && format != "json"
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "format must be json or html"})),
        )
            .into_response();
    }

    // Serve embedded HTML when explicitly requested.
    if query.format.as_deref() == Some("html") {
        let html = gallery_html();
        return (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )],
            html,
        )
            .into_response();
    }

    match service.list(query).await {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(GalleryError::InvalidQuery(msg)) => {
            (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

/// `GET /gallery/{id}/download` — return original stored bytes with MIME type.
pub async fn download_gallery_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Some(service) = &state.gallery_service else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "gallery not available"})),
        )
            .into_response();
    };

    match service.download(&id).await {
        Ok((bytes, metadata)) => {
            let content_type = metadata
                .mime_type
                .parse::<HeaderValue>()
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, content_type),
                    (
                        header::CONTENT_LENGTH,
                        HeaderValue::from(bytes.len() as u64),
                    ),
                ],
                Body::from(bytes),
            )
                .into_response()
        }
        Err(GalleryError::NotFound(id)) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("gallery item not found: {id}")})),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

/// `DELETE /gallery/{id}` — delete a gallery item.
pub async fn delete_gallery_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Some(service) = &state.gallery_service else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "gallery not available"})),
        )
            .into_response();
    };

    match service.delete(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"deleted": id}))).into_response(),
        Err(GalleryError::NotFound(id)) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("gallery item not found: {id}")})),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}
