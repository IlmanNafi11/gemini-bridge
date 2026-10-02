use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use gemini_bridge_llm_service::{
    Completion, CompletionSummary, LlmAdapter, LlmError, LlmEvent, LlmEventStream, LlmRequest,
};

pub struct LocalAdapter {
    pub id: &'static str,
    pub response: &'static str,
}

#[async_trait]
impl LlmAdapter for LocalAdapter {
    fn provider_id(&self) -> &'static str {
        self.id
    }

    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        Ok(Completion {
            text: self.response.to_owned(),
            finish_reason: "stop".to_owned(),
            usage: None,
            metadata: None,
        })
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        Ok(Box::pin(stream::iter([
            Ok(LlmEvent::TextDelta(self.response.to_owned())),
            Ok(LlmEvent::Completed(CompletionSummary {
                finish_reason: "stop".to_owned(),
                usage: None,
                metadata: None,
            })),
        ])))
    }
}

pub fn request(provider: &str) -> Arc<LlmRequest> {
    Arc::new(LlmRequest {
        model: gemini_bridge_llm_service::ModelSelector {
            provider: provider.to_owned(),
            model: "local-model".to_owned(),
            thinking_level: None,
        },
        messages: Arc::from([]),
        temperature: None,
        max_output_tokens: None,
        tools: Arc::from([]),
        metadata: Default::default(),
    })
}
