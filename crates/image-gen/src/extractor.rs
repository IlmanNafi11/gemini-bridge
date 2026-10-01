//! Extraction of generated image URLs from Gemini response payloads.
//!
//! Two strategies, applied in order:
//! 1. Positional/graph walk over a parsed JSON payload collecting any string
//!    that matches the `googleusercontent.com` image-URL pattern.
//! 2. Regex fallback over raw response text.
//!
//! Both deduplicate results while preserving first-seen order.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

static URL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://[^\s"']*googleusercontent\.com/[^\s"']+"#)
        .expect("image URL regex is valid")
});

/// Extract generated image URLs from a parsed Gemini response payload.
///
/// Walks every string value (including JSON-encoded nested strings) and
/// collects URLs matching the image pattern. Returns an empty vector when the
/// payload contains no such URL.
pub struct ImageExtractor;

impl ImageExtractor {
    /// Walk `payload` recursively and return deduplicated image URLs.
    pub fn extract_image_urls(payload: &Value) -> Vec<String> {
        let mut found = Vec::new();
        let mut seen = std::collections::HashSet::new();
        collect(payload, &mut found, &mut seen);
        found
    }

    /// Regex fallback: scan raw text for `googleusercontent.com` URLs.
    pub fn regex_fallback(raw_text: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for capture in URL_PATTERN.find_iter(raw_text) {
            let url = capture.as_str().to_owned();
            if seen.insert(url.clone()) {
                found.push(url);
            }
        }
        found
    }
}

fn collect(value: &Value, found: &mut Vec<String>, seen: &mut std::collections::HashSet<String>) {
    match value {
        Value::String(text) => {
            for url in ImageExtractor::regex_fallback(text) {
                if seen.insert(url.clone()) {
                    found.push(url);
                }
            }
            // A string may itself be JSON-encoded; descend if it parses.
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
            "candidates": [{
                "parts": [{
                    "text": format!("Here is your image: {IMAGE_URL}")
                }]
            }]
        });

        let urls = ImageExtractor::extract_image_urls(&payload);

        assert_eq!(urls, vec![IMAGE_URL.to_string()]);
    }

    #[test]
    fn returns_empty_for_payload_without_images() {
        let payload = json!({
            "candidates": [{ "parts": [{ "text": "No images here, sorry." }] }]
        });

        assert!(ImageExtractor::extract_image_urls(&payload).is_empty());
    }

    #[test]
    fn regex_fallback_extracts_all_urls_without_false_positives() {
        let raw = format!(
            "img1 {IMAGE_URL} and unrelated https://example.com/not-an-image.png \
             and img2 https://lh3.googleusercontent.com/second/def456=s512"
        );

        let urls = ImageExtractor::regex_fallback(&raw);

        assert_eq!(
            urls,
            vec![
                IMAGE_URL.to_string(),
                "https://lh3.googleusercontent.com/second/def456=s512".to_string(),
            ]
        );
        assert!(!urls.iter().any(|u| u.contains("example.com")));
    }

    #[test]
    fn deduplicates_repeated_urls() {
        let payload = json!([IMAGE_URL, IMAGE_URL]);

        assert_eq!(
            ImageExtractor::extract_image_urls(&payload),
            vec![IMAGE_URL.to_string()]
        );
    }

    #[test]
    fn descends_into_json_encoded_strings() {
        let inner = json!({ "url": IMAGE_URL }).to_string();
        let payload = json!({ "text": inner });

        assert_eq!(
            ImageExtractor::extract_image_urls(&payload),
            vec![IMAGE_URL.to_string()]
        );
    }
}
