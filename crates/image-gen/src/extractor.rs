use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use url::Url;

static URL_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s\"'\\]+"#).expect("image URL regex is valid"));

/// Extract generated image URLs from a parsed Gemini response payload.
///
/// Every candidate is parsed as a URL and accepted only when it uses HTTPS,
/// has no credentials, and targets `googleusercontent.com` itself or one of
/// its dot-delimited subdomains. This rejects host-substring confusion such as
/// `evil-googleusercontent.com` and `googleusercontent.com.evil.test`.
pub struct ImageExtractor;

impl ImageExtractor {
    /// Walk `payload` recursively and return deduplicated image URLs.
    pub fn extract_image_urls(payload: &Value) -> Vec<String> {
        let mut found = Vec::new();
        let mut seen = std::collections::HashSet::new();
        collect(payload, &mut found, &mut seen);
        found
    }

    /// Scan raw text for valid provider CDN image URLs.
    pub fn regex_fallback(raw_text: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for capture in URL_PATTERN.find_iter(raw_text) {
            let candidate = capture.as_str().trim_end_matches([',', ')', ']', '}']);
            if !is_allowed_image_url(candidate) {
                continue;
            }
            let url = candidate.to_owned();
            if seen.insert(url.clone()) {
                found.push(url);
            }
        }
        found
    }
}

fn is_allowed_image_url(candidate: &str) -> bool {
    let Ok(url) = Url::parse(candidate) else {
        return false;
    };
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "googleusercontent.com" || host.ends_with(".googleusercontent.com")
}

fn collect(value: &Value, found: &mut Vec<String>, seen: &mut std::collections::HashSet<String>) {
    match value {
        Value::String(text) => {
            for url in ImageExtractor::regex_fallback(text) {
                if seen.insert(url.clone()) {
                    found.push(url);
                }
            }
            if let Ok(child) = serde_json::from_str::<Value>(text) {
                collect(&child, found, seen);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect(item, found, seen);
            }
        }
        Value::Object(map) => {
            for child in map.values() {
                collect(child, found, seen);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const IMAGE_URL: &str = "https://lh3.googleusercontent.com/generated/abc123=s512";

    #[test]
    fn extracts_image_url_from_nested_payload() {
        let payload = json!({
            "candidates": [{ "parts": [{ "text": format!("Here is your image: {IMAGE_URL}") }] }]
        });
        assert_eq!(
            ImageExtractor::extract_image_urls(&payload),
            vec![IMAGE_URL.to_owned()]
        );
    }

    #[test]
    fn returns_empty_for_payload_without_images() {
        assert!(ImageExtractor::extract_image_urls(&json!({ "text": "No image" })).is_empty());
    }

    #[test]
    fn extracts_multiple_valid_urls_and_ignores_unrelated_hosts() {
        let raw = format!(
            "img1 {IMAGE_URL} unrelated https://example.com/no.png img2 https://lh3.googleusercontent.com/two.png"
        );
        assert_eq!(
            ImageExtractor::regex_fallback(&raw),
            vec![
                IMAGE_URL.to_owned(),
                "https://lh3.googleusercontent.com/two.png".to_owned()
            ]
        );
    }

    #[test]
    fn rejects_http_credentials_and_confused_hosts() {
        let payload = json!([
            "http://lh3.googleusercontent.com/image.png",
            "https://user:pass@lh3.googleusercontent.com/image.png",
            "https://evil-googleusercontent.com/image.png",
            "https://googleusercontent.com.evil.test/image.png",
            "https://lh3.googleusercontent.com@evil.test/image.png",
            "https://lh3.googleusercontent.com/image.png"
        ]);
        assert_eq!(
            ImageExtractor::extract_image_urls(&payload),
            vec!["https://lh3.googleusercontent.com/image.png".to_owned()]
        );
    }

    #[test]
    fn deduplicates_and_descends_into_json_encoded_strings() {
        let inner = json!({ "url": IMAGE_URL }).to_string();
        let payload = json!([IMAGE_URL, IMAGE_URL, inner]);
        assert_eq!(
            ImageExtractor::extract_image_urls(&payload),
            vec![IMAGE_URL.to_owned()]
        );
    }
}
