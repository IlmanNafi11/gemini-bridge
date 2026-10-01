use std::time::{Duration, SystemTime};

/// Configuration for a token-bucket rate limiter.
#[derive(Debug, Clone, Copy)]
pub struct TokenBucketConfig {
    /// Maximum number of tokens (burst capacity).
    pub capacity: u32,
    /// Tokens added per refill interval.
    pub refill_tokens: u32,
    /// Duration between refills.
    pub refill_interval: Duration,
}

/// The outcome of a rate-limit admission check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionDecision {
    Allow,
    Reject {
        /// Always 429 for rate-limiting.
        status: u16,
        /// Positive duration until the bucket has a token again.
        retry_after: Duration,
        /// Always `"rate_limit_exceeded"`.
        error_type: &'static str,
        /// Always `"rate_limit_exceeded"`.
        error_code: &'static str,
    },
}

/// Per-`client_id` token-bucket admission controller.
///
/// Calls update a shared bucket map under a short mutex, so concurrent attempts
/// cannot oversubscribe tokens.
///
/// Refill time and last access are tracked separately: frequent requests do not
/// postpone refill, and idle cleanup reflects actual client activity.
pub struct TokenBucketLimiter {
    config: TokenBucketConfig,
    buckets: parking_lot::Mutex<std::collections::HashMap<String, BucketState>>,
}

struct BucketState {
    tokens: u32,
    last_refill: SystemTime,
    last_access: SystemTime,
}

impl TokenBucketLimiter {
    pub fn new(config: TokenBucketConfig) -> Self {
        Self {
            config,
            buckets: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Try to acquire one token for `client_id` at `now`.
    ///
    /// Returns [`AdmissionDecision::Allow`] and decrements the token count, or
    /// [`AdmissionDecision::Reject`] with a positive `retry_after`.
    pub fn try_acquire(&self, client_id: &str, now: SystemTime) -> AdmissionDecision {
        let mut guard = self.buckets.lock();
        let cfg = self.config;

        let state = guard
            .entry(client_id.to_owned())
            .or_insert_with(|| BucketState {
                tokens: cfg.capacity,
                last_refill: now,
                last_access: now,
            });

        if !cfg.refill_interval.is_zero() {
            let elapsed = now
                .duration_since(state.last_refill)
                .unwrap_or(Duration::ZERO);
            let intervals = elapsed.as_nanos() / cfg.refill_interval.as_nanos();
            if intervals > 0 {
                let refill_count = intervals
                    .saturating_mul(u128::from(cfg.refill_tokens))
                    .min(u128::from(cfg.capacity)) as u32;
                state.tokens = state.tokens.saturating_add(refill_count).min(cfg.capacity);
                if intervals > u128::from(u32::MAX) {
                    state.last_refill = now;
                } else if let Some(refilled_through) = state
                    .last_refill
                    .checked_add(cfg.refill_interval * intervals as u32)
                {
                    state.last_refill = refilled_through;
                }
            }
        }
        state.last_access = now;

        if state.tokens > 0 {
            state.tokens -= 1;
            AdmissionDecision::Allow
        } else {
            // Time until the next token refill from when the bucket was last refilled.
            let since_refill = now
                .duration_since(state.last_refill)
                .unwrap_or(Duration::ZERO);
            let retry_after = cfg.refill_interval.saturating_sub(since_refill);
            let retry_after = if retry_after == Duration::ZERO {
                cfg.refill_interval
            } else {
                retry_after
            };
            AdmissionDecision::Reject {
                status: 429,
                retry_after,
                error_type: "rate_limit_exceeded",
                error_code: "rate_limit_exceeded",
            }
        }
    }

    /// Remove bucket state for clients whose last access is older than
    /// `older_than`.  Recently-used buckets (last_access >= older_than) are
    /// retained unchanged.
    pub fn remove_idle(&self, older_than: SystemTime) {
        let mut guard = self.buckets.lock();
        guard.retain(|_, state| state.last_access >= older_than);
    }
}
