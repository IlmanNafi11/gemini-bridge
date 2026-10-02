use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Extension, Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use gemini_bridge_middleware::HttpMetrics;
/// Expose accumulated request metrics in Prometheus text format.
pub async fn scrape(Extension(metrics): Extension<Arc<HttpMetrics>>) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics.render(),
    )
        .into_response()
}

/// Record requests and responses while keeping labels normalized and bounded.
pub async fn collect_requests(
    State(metrics): State<Option<Arc<HttpMetrics>>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(metrics) = metrics else {
        return next.run(request).await;
    };
    if request.uri().path() == "/metrics" {
        return next.run(request).await;
    }
    let labels = HttpMetrics::labels(request.method().as_str(), request.uri().path());
    let started = Instant::now();
    let response = next.run(request).await;
    metrics.record(labels, response.status().as_u16(), started.elapsed());
    response
}
