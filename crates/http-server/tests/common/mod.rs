//! Shared helpers for the `gemini-bridge-http-server` integration tests.
//!
//! V.4 (SPEC-session.md §7.5): exposes a create-session helper so every HTTP
//! test can drive the vault path (`POST /v1/sessions` → token) without
//! duplicating the flow. Tests that need it declare
//! `mod common;` and call [`create_session`].

#![allow(dead_code)]

use serde_json::{Value, json};

/// Cookie header that parses and passes a probe against a permissive upstream.
pub const COOKIE_OK: &str = "__Secure-1PSID=psid-a; __Secure-1PSIDTS=ts-a; SAPISID=sapisid-a";

/// Create a vault session through the public API.
///
/// Returns the parsed response body; the caller asserts on `id`, `token`, and
/// `session_status` itself. `label` is optional.
pub async fn create_session(
    base_url: &str,
    api_key: &str,
    cookie: &str,
    label: Option<&str>,
) -> (reqwest::StatusCode, Value) {
    let mut body = json!({ "cookie": cookie });
    if let Some(label) = label {
        body["label"] = json!(label);
    }
    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/sessions"))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .expect("create-session request succeeds");
    let status = response.status();
    let parsed: Value = response
        .json()
        .await
        .unwrap_or(json!({"parse_error": true}));
    (status, parsed)
}
