use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

pub const REDACTED_MARKER: &str = "***REDACTED***";

// Regex for cookie names that carry session credentials.
// Matches `__Secure-1PSID=...`, `__Secure-1PSIDTS=...`, `__Secure-1PSIDCC=...`, `SAPISID=...`
static COOKIE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(__Secure-1PSID(?:TS|CC)?|SAPISID)=([^\s;]+)").unwrap());

// Regex for Authorization header bearer tokens: `Bearer <token>`
static BEARER_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(Bearer\s+)([A-Za-z0-9_\-\.\~]+)").unwrap());

// Regex for SAPISIDHASH headers: `SAPISIDHASH <hash>`
static SAPISIDHASH_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(SAPISIDHASH\s+)([A-Za-z0-9_\-\.\~]+)").unwrap());

// Case-insensitive list of key names that contain secrets in JSON payloads.
const SENSITIVE_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "token",
    "password",
    "secret",
    "cookie",
    "authorization",
    "proxy_authorization",
];

/// Redaction utility to ensure credentials never appear in log or audit output.
#[derive(Debug, Clone, Default)]
pub struct RedactionFilter;

impl RedactionFilter {
    /// Returns `input` with all known credential values replaced by `***REDACTED***`.
    ///
    /// Preserves normal text, headers, and cookie keys. Idempotent.
    pub fn redact_str(input: &str) -> String {
        // Redact cookies: keep name, replace value.
        let after_cookies = COOKIE_PATTERN.replace_all(input, |caps: &regex::Captures| {
            format!("{}={}", &caps[1], REDACTED_MARKER)
        });

        // Redact Bearer tokens: keep "Bearer ", replace token.
        let after_bearer = BEARER_PATTERN.replace_all(&after_cookies, |caps: &regex::Captures| {
            format!("{}{}", &caps[1], REDACTED_MARKER)
        });

        // Redact SAPISIDHASH: keep header name, replace hash.
        let after_sapisidhash = SAPISIDHASH_PATTERN
            .replace_all(&after_bearer, |caps: &regex::Captures| {
                format!("{}{}", &caps[1], REDACTED_MARKER)
            });

        after_sapisidhash.into_owned()
    }

    /// Recursively redacts sensitive keys in JSON structures.
    pub fn redact_json(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (k, v) in map {
                    let k_lower = k.to_ascii_lowercase();
                    if SENSITIVE_KEYS.iter().any(|&s| k_lower.contains(s)) {
                        out.insert(k.clone(), Value::String(REDACTED_MARKER.to_string()));
                    } else {
                        out.insert(k.clone(), Self::redact_json(v));
                    }
                }
                Value::Object(out)
            }
            Value::Array(arr) => {
                let out = arr.iter().map(Self::redact_json).collect();
                Value::Array(out)
            }
            Value::String(s) => Value::String(Self::redact_str(s)),
            other => other.clone(),
        }
    }
}
