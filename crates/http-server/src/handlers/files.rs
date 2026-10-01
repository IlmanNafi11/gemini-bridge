//! File upload and retrieval routes.

use axum::Json;
use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde_json::json;

use crate::AppState;
use gemini_bridge_upload::UploadError;

pub async fn create_file(State(state): State<AppState>, mut multipart: Multipart) -> Response {
    let Some(service) = state.upload_service else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"file upload is unavailable"})),
        )
            .into_response();
    };

    let mut file_bytes = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"invalid multipart request"})),
                )
                    .into_response();
            }
        };
        if field.name() != Some("file") {
            continue;
        }
        let claimed_mime = field.content_type().map(ToOwned::to_owned);
        let mut bytes = Vec::new();
        let limit = service.max_bytes();
        let mut field = field;
        loop {
            let chunk = match field.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error":"invalid multipart file body"})),
                    )
                        .into_response();
                }
            };
            let received = bytes.len().saturating_add(chunk.len()) as u64;
            if received > limit {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(
                        json!({"error": format!("file exceeds configured limit of {limit} bytes")}),
                    ),
                )
                    .into_response();
            }
            bytes.extend_from_slice(&chunk);
        }
        file_bytes = Some((Bytes::from(bytes), claimed_mime));
        break;
    }

    let Some((bytes, claimed_mime)) = file_bytes else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"multipart field 'file' is required"})),
        )
            .into_response();
    };
    match service.upload_bytes(bytes, claimed_mime.as_deref()).await {
        Ok(uploaded) => (StatusCode::OK, Json(uploaded)).into_response(),
        Err(error) => upload_error_response(error),
    }
}

pub async fn get_file(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(service) = state.upload_service else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"file retrieval is unavailable"})),
        )
            .into_response();
    };
    match service.get(&id).await {
        Ok(bytes) => {
            let mime = service
                .get_by_id_mime(&id)
                .await
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let mut response = Response::new(Body::from(bytes));
            *response.status_mut() = StatusCode::OK;
            if let Ok(value) = HeaderValue::from_str(&mime) {
                response.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            response
        }
        Err(error) => upload_error_response(error),
    }
}

fn upload_error_response(error: UploadError) -> Response {
    let (status, message) = match error {
        UploadError::TooLarge { limit, .. } => (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("file exceeds configured limit of {limit} bytes"),
        ),
        UploadError::UnsupportedType(_) => (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported file type".into(),
        ),
        UploadError::NotFound(_) => (StatusCode::NOT_FOUND, "file not found".into()),
        UploadError::SsrfDenied(_)
        | UploadError::DnsResolutionFailed(_)
        | UploadError::InvalidUrl(_) => (
            StatusCode::BAD_REQUEST,
            "reference URL is not allowed".into(),
        ),
        UploadError::PushInitFailed(_) | UploadError::PushUploadFailed(_) => {
            (StatusCode::BAD_GATEWAY, "upstream upload failed".into())
        }
        UploadError::StoreError(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "media storage failed".into(),
        ),
    };
    (status, Json(json!({"error": message}))).into_response()
}
