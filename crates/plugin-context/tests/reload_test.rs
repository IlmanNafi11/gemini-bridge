use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gemini_bridge_plugin_context::{PluginError, ReloadableSlot};

#[tokio::test]
async fn reload_disposes_old_generation_only_after_its_last_request_handle_drops() {
    let disposed = Arc::new(AtomicUsize::new(0));
    let disposed_on_drop = Arc::clone(&disposed);
    let old = ReloadableSlot::new_with_disposer(
        Arc::new("old".to_owned()),
        Some(Box::new(move || {
            disposed_on_drop.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })),
    );

    let in_flight = old.get().await;
    assert_eq!(&**in_flight, "old");

    let replaced = old.publish(Arc::new("new".to_owned()));
    assert_eq!(&**old.get().await, "new");
    assert_eq!(disposed.load(Ordering::SeqCst), 0);

    drop(replaced);
    assert_eq!(disposed.load(Ordering::SeqCst), 0);

    drop(in_flight);
    assert_eq!(disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reload_disposer_failure_does_not_interrupt_new_generation() {
    let slot = ReloadableSlot::new_with_disposer(
        Arc::new(1u32),
        Some(Box::new(|| {
            Err(PluginError::SetupFailed("dispose failed".into()))
        })),
    );

    let old_generation = slot.publish(Arc::new(2u32));
    assert_eq!(*slot.get().await, 2);
    drop(old_generation);
    assert_eq!(*slot.get().await, 2);
}
