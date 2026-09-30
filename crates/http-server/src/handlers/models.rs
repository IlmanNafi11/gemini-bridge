//! Handler for `GET /v1/models`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use gemini_bridge_openai_compat::models::list_models;

pub async fn list_models_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(list_models()))
}
