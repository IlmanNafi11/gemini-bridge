use crate::{AdminStatusResponse, HealthResponse};

/// Render a dependency-free operator dashboard from local service snapshots.
pub fn render_dashboard(health: &HealthResponse, status: &AdminStatusResponse) -> String {
    let build_label = status.build_label.as_deref().unwrap_or("unavailable");
    let cookie_age = status
        .cookie_age_secs
        .map(|age| format!("{age} seconds"))
        .unwrap_or_else(|| "unknown".to_string());

    format!(
        "<!doctype html>\
         <html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Gemini Bridge Status</title>\
         <style>body{{font:16px system-ui,sans-serif;max-width:42rem;margin:3rem auto;padding:0 1rem;color:#172033}}\
         main{{border:1px solid #ccd3df;border-radius:.75rem;padding:1.5rem}}\
         dl{{display:grid;grid-template-columns:max-content 1fr;gap:.5rem 1rem}}dt{{font-weight:700}}dd{{margin:0}}</style>\
         </head><body><main><h1>Gemini Bridge Status</h1><dl>\
         <dt>Process</dt><dd>Process: {}</dd>\
         <dt>Version</dt><dd>{}</dd>\
         <dt>Uptime</dt><dd>{} seconds</dd>\
         <dt>Session</dt><dd>Session: {}</dd>\
         <dt>Build label</dt><dd>Build label: {}</dd>\
         <dt>Cookie age</dt><dd>{}</dd>\
         </dl></main></body></html>",
        escape_html(health.status),
        escape_html(health.version),
        health.uptime_secs,
        escape_html(&status.session_status),
        escape_html(build_label),
        cookie_age,
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::render_dashboard;
    use crate::{AdminStatusResponse, HealthResponse};

    #[test]
    fn dashboard_escapes_local_snapshot_values() {
        let html = render_dashboard(
            &HealthResponse {
                status: "ok",
                uptime_secs: 4,
                version: "0.1.0",
            },
            &AdminStatusResponse {
                session_status: "valid".to_string(),
                build_label: Some("<script>alert('x')</script>".to_string()),
                cookie_age_secs: None,
                last_valid_at: None,
                ip_flagged: false,
                checked_at: 0,
            },
        );

        assert!(html.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert"));
    }
}
