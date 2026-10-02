#[path = "mock_adapter.rs"]
mod mock_adapter;

use mock_adapter::LocalAdapter;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use gemini_bridge_llm_service::{
    AdapterRegistry, Completion, CompletionSummary, ContentPart, LlmAdapter, LlmError, LlmEvent,
    LlmEventStream, LlmRequest, LlmRouter, Message, ModelSelector, Role, ToolCall, Usage,
};

struct ContractAdapter;

#[async_trait]
impl LlmAdapter for ContractAdapter {
    fn provider_id(&self) -> &'static str {
        "contract-test"
    }

    async fn complete(&self, request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        let text = request
            .messages
            .iter()
            .flat_map(|message| message.parts.iter())
            .filter_map(|part| match part {
                ContentPart::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        Ok(Completion {
            text,
            finish_reason: "stop".to_owned(),
            usage: Some(Usage {
                prompt_tokens: 2,
                completion_tokens: 1,
            }),
            metadata: None,
        })
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        let events = vec![
            Ok(LlmEvent::TextDelta("hello".to_owned())),
            Ok(LlmEvent::Completed(CompletionSummary {
                finish_reason: "stop".to_owned(),
                usage: None,
                metadata: None,
            })),
        ];
        Ok(Box::pin(stream::iter(events)))
    }
}

struct ContractRouter(Arc<dyn LlmAdapter>);

impl LlmRouter for ContractRouter {
    fn adapter_for(&self, model: &ModelSelector) -> Result<Arc<dyn LlmAdapter>, LlmError> {
        if model.provider == self.0.provider_id() {
            Ok(Arc::clone(&self.0))
        } else {
            Err(LlmError::Unsupported("provider"))
        }
    }
}

fn request() -> Arc<LlmRequest> {
    Arc::new(LlmRequest {
        model: ModelSelector {
            provider: "contract-test".to_owned(),
            model: "test-model".to_owned(),
            thinking_level: None,
        },
        messages: Arc::from([Message {
            role: Role::User,
            parts: Arc::from([ContentPart::Text("hello".to_owned())]),
        }]),
        temperature: Some(0.2),
        max_output_tokens: Some(32),
        tools: Arc::from([]),
        metadata: BTreeMap::new(),
    })
}

#[tokio::test]
async fn router_selects_adapter_and_completion_preserves_contract_result() {
    let expected = request();
    let router = ContractRouter(Arc::new(ContractAdapter));
    let adapter = router.adapter_for(&expected.model).unwrap();
    let completion = adapter.complete(Arc::clone(&expected)).await.unwrap();

    assert_eq!(adapter.provider_id(), "contract-test");
    assert_eq!(completion.text, "hello");
    assert_eq!(completion.finish_reason, "stop");
    assert_eq!(completion.usage.unwrap().completion_tokens, 1);
    assert_eq!(expected.messages[0].role, Role::User);
    assert!(matches!(
        router.adapter_for(&ModelSelector {
            provider: "other".to_owned(),
            model: "model".to_owned(),
            thinking_level: None,
        }),
        Err(LlmError::Unsupported("provider"))
    ));
}

#[tokio::test]
async fn adapter_stream_emits_text_then_completion_summary() {
    let adapter = ContractAdapter;
    let events = adapter
        .stream(request())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], Ok(LlmEvent::TextDelta(text)) if text == "hello"));
    assert!(matches!(
        &events[1],
        Ok(LlmEvent::Completed(summary)) if summary.finish_reason == "stop"
    ));
}

#[test]
fn provider_neutral_error_taxonomy_is_constructible() {
    let errors = [
        LlmError::Authentication,
        LlmError::RateLimited,
        LlmError::Unavailable,
        LlmError::Unsupported("streaming"),
        LlmError::ContinuityRejected,
        LlmError::Protocol("bad response".to_owned()),
    ];
    assert_eq!(errors[3].to_string(), "unsupported capability: streaming");
    assert_eq!(
        errors[4].to_string(),
        "provider rejected conversation continuation identifiers"
    );

    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "lookup".to_owned(),
        arguments: "{}".to_owned(),
    };
    assert_eq!(call.name, "lookup");
}

#[test]
fn shared_request_is_immutable_and_clone_is_arc_backed() {
    let original = request();
    let shared = Arc::clone(&original);
    assert!(Arc::ptr_eq(&original, &shared));
    assert_eq!(original.messages[0].parts.len(), 1);
}

#[test]
fn json_metadata_remains_provider_neutral() {
    let metadata = serde_json::json!({"trace": "opaque-extension"});
    assert_eq!(metadata["trace"], "opaque-extension");
}
#[tokio::test]
async fn registry_routes_completions_to_two_provider_adapters() {
    let registry = AdapterRegistry::new();
    registry.register(Arc::new(ContractAdapter));
    registry.register(Arc::new(LocalAdapter {
        id: "second-test",
        response: "second adapter",
    }));

    let first = registry
        .adapter_for(&request().model)
        .unwrap()
        .complete(request())
        .await
        .unwrap();
    let second_request = mock_adapter::request("second-test");
    let second = registry
        .adapter_for(&second_request.model)
        .unwrap()
        .complete(second_request)
        .await
        .unwrap();

    assert_eq!(first.text, "hello");
    assert_eq!(second.text, "second adapter");
}

#[tokio::test]
async fn registry_preserves_stream_events_from_the_selected_provider() {
    let registry = AdapterRegistry::new();
    registry.register(Arc::new(LocalAdapter {
        id: "second-test",
        response: "second stream",
    }));
    let request = mock_adapter::request("second-test");

    let events = registry
        .stream(request)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert!(
        matches!(&events[..], [Ok(LlmEvent::TextDelta(text)), Ok(LlmEvent::Completed(_))] if text == "second stream")
    );
}

#[test]
fn registry_returns_provider_neutral_error_for_unknown_provider() {
    let registry = AdapterRegistry::new();
    let result = registry.adapter_for(&ModelSelector {
        provider: "unknown-provider".to_owned(),
        model: "model".to_owned(),
        thinking_level: None,
    });

    assert!(matches!(result, Err(LlmError::Unsupported("provider"))));
}
