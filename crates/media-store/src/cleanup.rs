//! TTL and expiry calculations for stored media.

use crate::metadata::MediaMetadata;
use std::time::{SystemTime, UNIX_EPOCH};

/// Return `true` if `metadata` has an `expires_at` timestamp that is strictly
/// less than `now_unix` (in seconds).
#[inline]
pub fn is_expired(metadata: &MediaMetadata, now_unix: i64) -> bool {
    matches!(metadata.expires_at, Some(exp) if exp < now_unix)
}

/// Helper to get current Unix timestamp in seconds.
#[inline]
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Compute an `expires_at` timestamp from `created_at` + `ttl_days`.
/// Returns `None` if `ttl_days == 0`.
pub fn compute_default_expiry(created_at: i64, ttl_days: u32) -> Option<i64> {
    if ttl_days == 0 {
        None
    } else {
        Some(created_at + (ttl_days as i64 * 86400))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_check_future() {
        let meta = MediaMetadata {
            id: "1".into(),
            sha256: "abc".into(),
            mime_type: "text/plain".into(),
            size_bytes: 0,
            created_at: 100,
            expires_at: Some(200),
            prompt: None,
            model: None,
        };
        assert!(!is_expired(&meta, 150));
        assert!(!is_expired(&meta, 200)); // exactly at expiration is not expired
        assert!(is_expired(&meta, 201)); // strictly after expiration
    }

    #[test]
    fn expiry_check_none() {
        let meta = MediaMetadata {
            id: "1".into(),
            sha256: "abc".into(),
            mime_type: "text/plain".into(),
            size_bytes: 0,
            created_at: 100,
            expires_at: None,
            prompt: None,
            model: None,
        };
        assert!(!is_expired(&meta, 999999));
    }

    #[test]
    fn compute_default_expiry_behavior() {
        assert_eq!(compute_default_expiry(1000, 0), None);
        assert_eq!(compute_default_expiry(1000, 30), Some(1000 + 30 * 86400));
    }
}
