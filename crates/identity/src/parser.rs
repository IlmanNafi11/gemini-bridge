use regex::Regex;
use sha1::{Digest, Sha1};

/// Extract the `bl` build-label from Gemini `/app` HTML.
///
/// Tries multiple patterns known to appear in the page source:
/// - `"cfb2h":"<value>"`
/// - `boq_assistant-bard-web-server_<timestamp>.<hash>` style token in script
pub fn extract_bl(html: &str) -> Option<String> {
    // Pattern 1: JSON field "cfb2h":"..."
    if let Some(v) = extract_quoted_field(html, "cfb2h") {
        return Some(v);
    }
    // Pattern 2: boq_assistant-bard-web-server_... token
    let re = Regex::new(r#"(boq_assistant-bard-web-server_[\w.-]+)"#).ok()?;
    re.captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract the `SNlM0e` anti-CSRF token from Gemini `/app` HTML.
///
/// Tries:
/// - `WIZ_global_data.SNlM0e = "<value>"`
/// - `["SNlM0e","<value>"]`
pub fn extract_snlm0e(html: &str) -> Option<String> {
    // Pattern 1: JSON field "SNlM0e":"..."
    if let Some(v) = extract_quoted_field(html, "SNlM0e") {
        return Some(v);
    }
    // Pattern 2: WIZ_global_data assignment
    let re1 = Regex::new(r#"WIZ_global_data\.SNlM0e\s*=\s*"([^"]+)""#).ok()?;
    if let Some(c) = re1.captures(html) {
        return c.get(1).map(|m| m.as_str().to_string());
    }
    // Pattern 3: JSON array form ["SNlM0e","..."]
    let re2 = Regex::new(r#"\["SNlM0e","([^"]+)"\]"#).ok()?;
    re2.captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract `f.sid` (session ID) from Gemini `/app` HTML.
///
/// Tries:
/// - `"FdrFJe":"<value>"`
/// - `FdrFJe` JSON field in various forms
pub fn extract_fsid(html: &str) -> Option<String> {
    extract_quoted_field(html, "FdrFJe")
}

/// Build the `Authorization: SAPISIDHASH <ts>_<sha1>` header value.
///
/// Formula: `sha1(timestamp + " " + sapisid + " " + "https://gemini.google.com")`
pub fn build_sapisidhash(sapisid: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    compute_sapisidhash(ts, sapisid)
}

/// Internal helper that is easily testable by injecting the timestamp.
pub fn compute_sapisidhash(ts: u64, sapisid: &str) -> String {
    let payload = format!("{} {} https://gemini.google.com", ts, sapisid);
    let hash = Sha1::digest(payload.as_bytes());
    format!("SAPISIDHASH {}_{}", ts, hex::encode(hash))
}

// ─── internal helpers ────────────────────────────────────────────────────────

/// Extract the string value of a JSON-like field `"<key>":"<value>"` from HTML.
fn extract_quoted_field(html: &str, key: &str) -> Option<String> {
    let pattern = format!(r#""{}":\s*"([^"\\]*(?:\\.[^"\\]*)*)""#, regex::escape(key));
    let re = Regex::new(&pattern).ok()?;
    re.captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

// hex encoding used in SAPISIDHASH
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}
