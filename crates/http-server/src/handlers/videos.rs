//! Video generation and retrieval route handlers.

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::AppState;
use gemini_bridge_adapter_video::{VideoError, VideoGenerationRequest, error_code, error_status};

/// `POST /v1/videos/generations`.
///
/// The route remains registered while the optional service is absent, so the
/// off-by-default behavior is an actionable 501 rather than an unrecognized 404.
pub async fn generate_video(
    State(state): State<AppState>,
    Json(payload): Json<VideoGenerationRequest>,
) -> Response {
    let Some(service) = state.video_service else {
        return error_response(VideoError::Disabled);
    };

    match service.generate(payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => error_response(error),
    }
}

/// `GET /v1/videos/{id}` retrieves a cached video by its local opaque ID.
pub async fn get_video(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(service) = state.video_service else {
        return error_response(VideoError::Disabled);
    };

    match service.get_video(&id).await {
        Ok((bytes, mime_type)) => {
            let mut response = (StatusCode::OK, Body::from(bytes)).into_response();
            if let Ok(content_type) = HeaderValue::from_str(&mime_type) {
                response
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, content_type);
            }
            response
        }
        Err(VideoError::MediaError(_)) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": format!("video {id} not found"),
                    "type": "invalid_request_error",
                    "code": "resource_not_found"
                }
            })),
        )
            .into_response(),
        Err(error) => error_response(error),
    }
}

fn error_response(error: VideoError) -> Response {
    let status = error_status(&error);
    let message = error.to_string();
    let code = error_code(&error);
    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": "video_generation_error",
                "code": code
            }
        })),
    )
        .into_response()
}
