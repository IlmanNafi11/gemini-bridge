use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Mutex;
use std::time::Duration;

const LATENCY_BUCKETS_SECONDS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RequestKey {
    method: &'static str,
    route: &'static str,
    status_class: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LatencyKey {
    method: &'static str,
    route: &'static str,
}

/// Pre-normalized bounded-cardinality labels captured before request dispatch.
#[derive(Debug, Clone, Copy)]
pub struct HttpMetricLabels {
    method: &'static str,
    route: &'static str,
}

#[derive(Default)]
struct Histogram {
    count: u64,
    sum_seconds: f64,
    cumulative_buckets: [u64; LATENCY_BUCKETS_SECONDS.len()],
}

#[derive(Default)]
struct MetricsState {
    requests: BTreeMap<RequestKey, u64>,
    errors: BTreeMap<RequestKey, u64>,
    latency: BTreeMap<LatencyKey, Histogram>,
}

/// Bounded-cardinality HTTP request, error, and latency metrics.
#[derive(Default)]
pub struct HttpMetrics {
    state: Mutex<MetricsState>,
}

impl HttpMetrics {
    /// Normalize request labels before the request is moved into the handler stack.
    pub fn labels(method: &str, path: &str) -> HttpMetricLabels {
        HttpMetricLabels {
            method: normalize_method(method),
            route: normalize_route(path),
        }
    }

    /// Record a completed request using pre-normalized bounded labels.
    pub fn record(&self, labels: HttpMetricLabels, status: u16, duration: Duration) {
        let status_class = normalize_status(status);
        let request_key = RequestKey {
            method: labels.method,
            route: labels.route,
            status_class,
        };
        let latency_key = LatencyKey {
            method: labels.method,
            route: labels.route,
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *state.requests.entry(request_key.clone()).or_default() += 1;
        if status >= 400 {
            *state.errors.entry(request_key).or_default() += 1;
        }

        let histogram = state.latency.entry(latency_key).or_default();
        let seconds = duration.as_secs_f64();
        histogram.count += 1;
        histogram.sum_seconds += seconds;
        for (index, bound) in LATENCY_BUCKETS_SECONDS.iter().enumerate() {
            if seconds <= *bound {
                histogram.cumulative_buckets[index] += 1;
            }
        }
    }

    /// Encode recorded metrics using the Prometheus text exposition format.
    pub fn render(&self) -> String {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut output = String::new();
        output.push_str(
            "# HELP gemini_bridge_http_requests_total Completed HTTP requests.\n\
             # TYPE gemini_bridge_http_requests_total counter\n",
        );
        for (key, count) in &state.requests {
            let _ = writeln!(
                output,
                "gemini_bridge_http_requests_total{{method=\"{}\",route=\"{}\",status_class=\"{}\"}} {count}",
                key.method, key.route, key.status_class
            );
        }

        output.push_str(
            "# HELP gemini_bridge_http_errors_total HTTP responses with a 4xx or 5xx status.\n\
             # TYPE gemini_bridge_http_errors_total counter\n",
        );
        for (key, count) in &state.errors {
            let _ = writeln!(
                output,
                "gemini_bridge_http_errors_total{{method=\"{}\",route=\"{}\",status_class=\"{}\"}} {count}",
                key.method, key.route, key.status_class
            );
        }

        output.push_str(
            "# HELP gemini_bridge_http_request_duration_seconds HTTP request duration in seconds.\n\
             # TYPE gemini_bridge_http_request_duration_seconds histogram\n",
        );
        for (key, histogram) in &state.latency {
            for (index, bound) in LATENCY_BUCKETS_SECONDS.iter().enumerate() {
                let _ = writeln!(
                    output,
                    "gemini_bridge_http_request_duration_seconds_bucket{{method=\"{}\",route=\"{}\",le=\"{}\"}} {}",
                    key.method, key.route, bound, histogram.cumulative_buckets[index]
                );
            }
            let _ = writeln!(
                output,
                "gemini_bridge_http_request_duration_seconds_bucket{{method=\"{}\",route=\"{}\",le=\"+Inf\"}} {}",
                key.method, key.route, histogram.count
            );
            let _ = writeln!(
                output,
                "gemini_bridge_http_request_duration_seconds_sum{{method=\"{}\",route=\"{}\"}} {:.9}",
                key.method, key.route, histogram.sum_seconds
            );
            let _ = writeln!(
                output,
                "gemini_bridge_http_request_duration_seconds_count{{method=\"{}\",route=\"{}\"}} {}",
                key.method, key.route, histogram.count
            );
        }
        output
    }
}

fn normalize_method(method: &str) -> &'static str {
    match method {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    }
}

fn normalize_status(status: u16) -> &'static str {
    match status / 100 {
        1 => "1xx",
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        5 => "5xx",
        _ => "other",
    }
}

fn normalize_route(path: &str) -> &'static str {
    match path {
        "/healthz" => "/healthz",
        "/readyz" => "/readyz",
        "/metrics" => "/metrics",
        "/admin/status" => "/admin/status",
        "/admin/dashboard" => "/admin/dashboard",
        "/admin/reauth" => "/admin/reauth",
        "/admin/reload-plugin" => "/admin/reload-plugin",
        "/admin/purge" => "/admin/purge",
        "/v1/chat/completions" => "/v1/chat/completions",
        "/v1/models" => "/v1/models",
        "/v1/files" => "/v1/files",
        "/v1/images/generations" => "/v1/images/generations",
        "/v1/videos/generations" => "/v1/videos/generations",
        "/v1/conversations" => "/v1/conversations",
        "/gallery" => "/gallery",
        _ => {
            let mut segments = path.split('/').filter(|segment| !segment.is_empty());
            match (
                segments.next(),
                segments.next(),
                segments.next(),
                segments.next(),
            ) {
                (Some("v1"), Some("files"), Some(_), None) => "/v1/files/{id}",
                (Some("v1"), Some("images"), Some(_), None) => "/v1/images/{id}",
                (Some("v1"), Some("videos"), Some(_), None) => "/v1/videos/{id}",
                (Some("v1"), Some("conversations"), Some(_), Some("messages"))
                    if segments.next().is_none() =>
                {
                    "/v1/conversations/{id}/messages"
                }
                (Some("v1"), Some("conversations"), Some(_), Some("branch"))
                    if segments.next().is_none() =>
                {
                    "/v1/conversations/{id}/branch"
                }
                (Some("v1"), Some("conversations"), Some(_), Some("regenerate"))
                    if segments.next().is_none() =>
                {
                    "/v1/conversations/{id}/regenerate"
                }
                (Some("gallery"), Some(_), None, None) => "/gallery/{id}",
                (Some("gallery"), Some(_), Some("download"), None) => "/gallery/{id}/download",
                _ => "unmatched",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HttpMetricLabels, HttpMetrics};
    use std::time::Duration;

    #[test]
    fn metrics_use_bounded_route_and_method_labels() {
        let metrics = HttpMetrics::default();
        metrics.record(
            HttpMetrics::labels("GET", "/v1/files/private-file-id?secret=x"),
            404,
            Duration::from_millis(12),
        );
        metrics.record(
            HttpMetricLabels {
                method: "OTHER",
                route: "unmatched",
            },
            500,
            Duration::from_secs(12),
        );

        let output = metrics.render();
        assert!(output.contains("method=\"GET\",route=\"/v1/files/{id}\",status_class=\"4xx\""));
        assert!(output.contains("method=\"OTHER\",route=\"unmatched\",status_class=\"5xx\""));
        assert!(!output.contains("private-file-id"));
        assert!(!output.contains("user-data"));
        assert!(output.contains("le=\"+Inf\""));
        assert!(output.contains(
            "gemini_bridge_http_request_duration_seconds_bucket{method=\"GET\",route=\"/v1/files/{id}\",le=\"0.025\"} 1"
        ));
        assert!(output.contains(
            "gemini_bridge_http_request_duration_seconds_sum{method=\"GET\",route=\"/v1/files/{id}\"} 0.012000000"
        ));
        assert!(output.contains(
            "gemini_bridge_http_request_duration_seconds_bucket{method=\"OTHER\",route=\"unmatched\",le=\"10\"} 0"
        ));
        assert!(output.contains(
            "gemini_bridge_http_request_duration_seconds_bucket{method=\"OTHER\",route=\"unmatched\",le=\"+Inf\"} 1"
        ));
    }
}
