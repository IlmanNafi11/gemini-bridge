#[path = "../crates/llm-service/tests/mock_adapter.rs"]
mod mock_adapter;

use std::sync::Arc;

use gemini_bridge_llm_service::{AdapterRegistry, LlmAdapter};
use gemini_bridge_plugin_context::PluginContext;
use mock_adapter::LocalAdapter;

#[tokio::test]
async fn plugin_registered_provider_neutral_registry_routes_two_local_adapters() {
    let registry = AdapterRegistry::new();
    registry.register(Arc::new(LocalAdapter {
        id: "first-local",
        response: "first adapter response",
    }));
    registry.register(Arc::new(LocalAdapter {
        id: "second-local",
        response: "second adapter response",
    }));

    let mut context = PluginContext::new();
    context.provide("llm-adapter", Arc::new(registry));
    let injected = context
        .inject::<AdapterRegistry>("llm-adapter")
        .expect("provider-neutral registry should be injectable");

    for (provider, expected) in [
        ("first-local", "first adapter response"),
        ("second-local", "second adapter response"),
    ] {
        let completion = injected
            .complete(mock_adapter::request(provider))
            .await
            .expect("registered provider should complete");
        assert_eq!(completion.text, expected);
    }
}

#[tokio::test]
async fn local_adapter_stream_uses_the_same_provider_neutral_registry() {
    use futures::StreamExt;
    use gemini_bridge_llm_service::LlmEvent;

    let registry = AdapterRegistry::new();
    registry.register(Arc::new(LocalAdapter {
        id: "second-local",
        response: "second adapter stream",
    }));
    let adapter: Arc<dyn LlmAdapter> = Arc::new(registry);
    let request = mock_adapter::request("second-local");

    let events = adapter
        .stream(request)
        .await
        .expect("registered provider should stream")
        .collect::<Vec<_>>()
        .await;

    assert!(
        matches!(&events[..], [Ok(LlmEvent::TextDelta(text)), Ok(LlmEvent::Completed(_))] if text == "second adapter stream")
    );
}
