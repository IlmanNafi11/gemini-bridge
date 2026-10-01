use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, SystemTime};

use gemini_bridge_middleware::{
    AdmissionDecision, AuditLogEntry, RedactionFilter, TokenBucketConfig, TokenBucketLimiter,
};
use serde_json::json;

#[test]
fn redacts_cookie_and_auth_values_but_preserves_unrelated_text() {
    let input =
        "Authorization: Bearer bearer-sentinel; __Secure-1PSID=cookie-sentinel; ordinary=text";
    let output = RedactionFilter::redact_str(input);

    assert!(!output.contains("bearer-sentinel"));
    assert!(!output.contains("cookie-sentinel"));
    assert!(output.contains("***REDACTED***"));
    assert!(output.contains("ordinary=text"));
    assert_eq!(RedactionFilter::redact_str(&output), output);
}

#[test]
fn recursively_redacts_case_insensitive_secret_fields() {
    let input = json!({
        "Api_Key": "key-sentinel",
        "nested": [{"PASSWORD": "password-sentinel", "visible": 7}]
    });
    let output = RedactionFilter::redact_json(&input);

    assert_eq!(output["Api_Key"], "***REDACTED***");
    assert_eq!(output["nested"][0]["PASSWORD"], "***REDACTED***");
    assert_eq!(output["nested"][0]["visible"], 7);
}

#[test]
fn token_bucket_enforces_capacity_refills_at_interval_and_separates_clients() {
    let start = SystemTime::UNIX_EPOCH;
    let limiter = TokenBucketLimiter::new(TokenBucketConfig {
        capacity: 2,
        refill_tokens: 1,
        refill_interval: Duration::from_secs(10),
    });

    assert_eq!(
        limiter.try_acquire("client-a", start),
        AdmissionDecision::Allow
    );
    assert_eq!(
        limiter.try_acquire("client-a", start),
        AdmissionDecision::Allow
    );
    assert!(
        matches!(limiter.try_acquire("client-a", start), AdmissionDecision::Reject { status: 429, retry_after, .. } if retry_after > Duration::ZERO)
    );
    assert_eq!(
        limiter.try_acquire("client-b", start),
        AdmissionDecision::Allow
    );
    assert_eq!(
        limiter.try_acquire("client-a", start + Duration::from_secs(10)),
        AdmissionDecision::Allow
    );
}

#[test]
fn concurrent_acquisition_never_exceeds_bucket_capacity() {
    let limiter = Arc::new(TokenBucketLimiter::new(TokenBucketConfig {
        capacity: 13,
        refill_tokens: 1,
        refill_interval: Duration::from_secs(60),
    }));
    let admitted = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for _ in 0..100 {
            let limiter = Arc::clone(&limiter);
            let admitted = Arc::clone(&admitted);
            scope.spawn(move || {
                if limiter.try_acquire("same-client", SystemTime::UNIX_EPOCH)
                    == AdmissionDecision::Allow
                {
                    admitted.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    assert_eq!(admitted.load(Ordering::Relaxed), 13);
}

#[test]
fn audit_entry_serializes_only_safe_request_metadata() {
    let entry = AuditLogEntry {
        request_id: "request-123".to_string(),
        timestamp: 1_800_000_000,
        method: "GET".to_string(),
        path: "/v1/models".to_string(),
        status: 200,
        duration_ms: 12,
        model: None,
    };
    let serialized = serde_json::to_string(&entry).unwrap();

    assert!(serialized.contains("request-123"));
    assert!(serialized.contains("/v1/models"));
    assert!(!serialized.contains("?"));
    assert!(!serialized.contains("sentinel"));
}

#[test]
fn idle_bucket_removal_preserves_recently_used_buckets() {
    let limiter = TokenBucketLimiter::new(TokenBucketConfig {
        capacity: 1,
        refill_tokens: 1,
        refill_interval: Duration::from_secs(60),
    });
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    limiter.try_acquire("recent", start);
    limiter.try_acquire("idle", start - Duration::from_secs(100));

    limiter.remove_idle(start - Duration::from_secs(50));

    assert!(matches!(
        limiter.try_acquire("recent", start),
        AdmissionDecision::Reject { .. }
    ));
    assert_eq!(limiter.try_acquire("idle", start), AdmissionDecision::Allow);
}
