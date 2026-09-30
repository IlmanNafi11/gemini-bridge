use gemini_bridge_plugin_context::{PluginContext, PluginError};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

// ── provide + inject happy path ──────────────────────────────────────────────

#[test]
fn provide_and_inject_correct_type_succeeds() {
    let mut ctx = PluginContext::new();
    ctx.provide::<String>("greeting", Arc::new("hello".to_string()));

    let val = ctx.inject::<String>("greeting").expect("should succeed");
    assert_eq!(val.as_str(), "hello");
}

// ── inject missing key ───────────────────────────────────────────────────────

#[test]
fn inject_missing_key_returns_missing_dependency() {
    let ctx = PluginContext::new();
    let err = ctx.inject::<String>("nope").unwrap_err();
    assert!(matches!(err, PluginError::MissingDependency("nope")));
}

// ── inject wrong type ────────────────────────────────────────────────────────

#[test]
fn inject_wrong_type_returns_missing_dependency() {
    let mut ctx = PluginContext::new();
    // Register a u64 under "num"
    ctx.provide::<u64>("num", Arc::new(42u64));

    // Try to retrieve it as a String — downcast must fail
    let err = ctx.inject::<String>("num").unwrap_err();
    assert!(matches!(err, PluginError::MissingDependency("num")));
}

// ── teardown LIFO order ──────────────────────────────────────────────────────

/// Encodes the LIFO order into a single integer: each disposer atomically
/// shifts in its index so the final value can be decoded, or we track a
/// generation counter.  Simpler: use an `Arc<AtomicU64>` as a bitmask.
///
/// We register disposers for indices 0, 1, 2. LIFO means they execute 2, 1, 0.
/// We pack execution order into bits: first caller sets bit 0, second sets
/// bit 1, etc.  Then we check the mapping.
#[tokio::test]
async fn teardown_runs_disposers_in_lifo_order() {
    // Shared counters: counter[i] stores the step number at which disposer i ran.
    let step = Arc::new(AtomicUsize::new(0));
    let counters: Vec<Arc<AtomicUsize>> = (0..3).map(|_| Arc::new(AtomicUsize::new(99))).collect();

    let mut ctx = PluginContext::new();

    for counter in &counters {
        let step_clone = Arc::clone(&step);
        let counter_clone = Arc::clone(counter);
        ctx.register_disposer(Box::new(move || {
            let s = step_clone.fetch_add(1, Ordering::SeqCst);
            counter_clone.store(s, Ordering::SeqCst);
            Ok(())
        }));
    }

    ctx.teardown().await.expect("teardown should succeed");

    // registered 0, 1, 2 → LIFO → disposer 2 runs first (step 0),
    //                                disposer 1 runs second (step 1),
    //                                disposer 0 runs third (step 2).
    assert_eq!(
        counters[2].load(Ordering::SeqCst),
        0,
        "disposer 2 should run first"
    );
    assert_eq!(
        counters[1].load(Ordering::SeqCst),
        1,
        "disposer 1 should run second"
    );
    assert_eq!(
        counters[0].load(Ordering::SeqCst),
        2,
        "disposer 0 should run third"
    );
}

// ── teardown with failing disposer ───────────────────────────────────────────

#[tokio::test]
async fn teardown_with_failing_disposer_returns_error() {
    let mut ctx = PluginContext::new();

    // First disposer (registered first, runs last in LIFO): succeeds
    ctx.register_disposer(Box::new(|| Ok(())));

    // Second disposer (registered second, runs first in LIFO): fails
    ctx.register_disposer(Box::new(|| {
        Err(PluginError::SetupFailed("boom".to_string()))
    }));

    let result = ctx.teardown().await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), PluginError::SetupFailed(_)));
}
