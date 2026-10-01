//! Image generation and retrieval route handlers.

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::AppState;
use gemini_bridge_image_gen::{ImageGenError, ImageGenerationRequest};

/// `POST /v1/images/generations`
///
/// Accepts an OpenAI-compatible [`ImageGenerationRequest`], forwards it to
/// the image generation service, and returns proxy URLs or base64 results.
pub async fn generate_image(
    State(state): State<AppState>,
    Json(payload): Json<ImageGenerationRequest>,
) -> Response {
    let Some(service) = state.image_service else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "error": {
                    "message": "image generation is not configured on this bridge instance",
                    "type": "not_implemented_error",
                    "code": "image_generation_disabled"
                }
            })),
        )
            .into_response();
    };

    match service.generate(payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(err) => image_error_response(err),
    }
}

/// `GET /v1/images/{id}`
///
/// Retrieves a previously generated and cached image by its local media ID.
pub async fn get_image(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(service) = state.image_service else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "error": {
                    "message": "image service is not configured",
                    "type": "not_implemented_error",
                    "code": "image_generation_disabled"
                }
            })),
        )
            .into_response();
    };

    match service.get_image(&id).await {
        Ok((bytes, mime_type)) => {
            let mut response = (StatusCode::OK, Body::from(bytes)).into_response();
            if let Ok(content_type) = HeaderValue::from_str(&mime_type) {
                response
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, content_type);
            }
            response
        }
        Err(ImageGenError::StoreError(_)) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": format!("image {id} not found"),
                    "type": "invalid_request_error",
                    "code": "resource_not_found"
                }
            })),
        )
            .into_response(),
        Err(err) => image_error_response(err),
    }
}

fn image_error_response(error: ImageGenError) -> Response {
    let (status, message, code) = match error {
        ImageGenError::Validation(msg) => (StatusCode::BAD_REQUEST, msg, "validation_error"),
        ImageGenError::UploadFailed(msg) => (
            StatusCode::BAD_REQUEST,
            format!("reference upload failed: {msg}"),
            "invalid_request_error",
        ),
        ImageGenError::NoImageExtracted => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "no image could be extracted from the model response".to_owned(),
            "no_image_extracted",
        ),
        ImageGenError::ImageDownloadFailed(msg) => (
            StatusCode::BAD_GATEWAY,
            format!("failed to download generated image from upstream: {msg}"),
            "upstream_download_error",
        ),
        ImageGenError::AdapterError(msg) => (
            StatusCode::BAD_GATEWAY,
            format!("upstream adapter error: {msg}"),
            "adapter_error",
        ),
        ImageGenError::StoreError(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("media storage error: {msg}"),
            "storage_error",
        ),
    };

    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": "image_generation_error",
                "code": code
            }
        })),
    )
        .into_response()
}
