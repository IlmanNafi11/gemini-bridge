//! End-to-end conversation continuity, branching, regeneration, and degraded replay.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use gemini_bridge_conversation_store::{ConversationStore, SqliteConversationStore};
use gemini_bridge_health_admin::DefaultHealthAdminService;
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_llm_service::{
    Completion, LlmAdapter, LlmError, LlmEventStream, LlmRequest, ProviderMetadata,
};
use serde_json::{Value, json};

#[derive(Default)]
struct RecordingAdapter {
    requests: Mutex<Vec<Arc<LlmRequest>>>,
    reject_next_continuation: Mutex<bool>,
}

impl RecordingAdapter {
    fn requests(&self) -> Vec<Arc<LlmRequest>> {
        self.requests.lock().unwrap().clone()
    }

    fn reject_next_continuation(&self) {
        *self.reject_next_continuation.lock().unwrap() = true;
    }
}

#[async_trait]
impl LlmAdapter for RecordingAdapter {
    fn provider_id(&self) -> &'static str {
        "recording"
    }

    async fn complete(&self, request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        self.requests.lock().unwrap().push(request.clone());
        if request.metadata.contains_key("conversation_id")
            && std::mem::take(&mut *self.reject_next_continuation.lock().unwrap())
        {
            return Err(LlmError::ContinuityRejected);
        }

        let call = self.requests.lock().unwrap().len();
        Ok(Completion {
            text: format!("assistant-{call}"),
            finish_reason: "stop".to_owned(),
            usage: None,
            metadata: Some(ProviderMetadata {
                raw: json!({
                    "conversation_id": "gemini-conversation",
                    "response_id": format!("response-{call}"),
                    "candidate_id": format!("candidate-{call}"),
                }),
            }),
        })
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        Err(LlmError::Unsupported("streaming"))
    }
}

async fn test_server() -> (
    String,
    tokio::task::JoinHandle<()>,
    Arc<RecordingAdapter>,
    Arc<SqliteConversationStore>,
) {
    let adapter = Arc::new(RecordingAdapter::default());
    let store = Arc::new(SqliteConversationStore::in_memory().unwrap());
    let state = AppState {
        adapter: adapter.clone(),
        upload_service: None,
        image_service: None,
        health_admin: Arc::new(DefaultHealthAdminService::new(None)),
        conversation_store: Some(store.clone()),
        tool_engine: gemini_bridge_http_server::build_tool_engine(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router: Router = build_router(
        ServerConfig {
            bind_addr: addr,
            api_key: None,
            require_key_for_admin: false,
            cors_enabled: false,
            rate_limit: None,
        },
        state,
    );
    let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), handle, adapter, store)
}

#[tokio::test]
async fn multi_turn_history_uses_upstream_ids_and_is_listed() {
    let (base, server, adapter, _store) = test_server().await;
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "first"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    let first_body: Value = first.json().await.unwrap();
    let conversation_id = first_body["gemini_metadata"]["conversation_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let second = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "conversation_id": conversation_id,
            "messages": [{"role": "user", "content": "second"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 200);
    assert_eq!(second.headers()["x-gemini-bridge-continuity"], "active");

    let recorded = adapter.requests();
    assert_eq!(recorded.len(), 2);
    assert_eq!(
        recorded[1].metadata["conversation_id"],
        "gemini-conversation"
    );
    assert_eq!(recorded[1].metadata["response_id"], "response-1");

    let list: Value = client
        .get(format!("{base}/v1/conversations"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["data"].as_array().unwrap().len(), 1);

    let history: Value = client
        .get(format!(
            "{base}/v1/conversations/{conversation_id}/messages"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let messages = history["data"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[3]["role"], "assistant");
    server.abort();
}

#[tokio::test]
async fn branch_and_regenerate_are_isolated_from_source_history() {
    let (base, server, _adapter, store) = test_server().await;
    let client = reqwest::Client::new();

    let conversation = store
        .create_conversation(Some("source".to_owned()))
        .await
        .unwrap();
    for (seq, role, content) in [
        (1, "user", "question"),
        (2, "assistant", "answer"),
        (3, "user", "follow-up"),
        (4, "assistant", "later"),
    ] {
        store
            .append_message(gemini_bridge_conversation_store::StoredMessage {
                id: format!("msg-{seq}"),
                conversation_id: conversation.id.clone(),
                parent_message_id: None,
                role: role.to_owned(),
                content_json: serde_json::to_string(content).unwrap(),
                sequence_number: seq,
                created_at: seq,
                upstream_conversation_id: Some("gemini-conversation".to_owned()),
                upstream_response_id: Some(format!("response-{seq}")),
                upstream_candidate_id: Some(format!("candidate-{seq}")),
            })
            .await
            .unwrap();
    }

    let branch: Value = client
        .post(format!(
            "{base}/v1/conversations/{}/branch",
            conversation.id
        ))
        .json(&json!({"from_message_id": "msg-2", "title": "alternate"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(branch["inherited_message_count"], 2);
    let branch_id = branch["conversation"]["id"].as_str().unwrap();

    let branch_history: Value = client
        .get(format!("{base}/v1/conversations/{branch_id}/messages"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let branch_message_id = branch_history["data"][1]["id"].as_str().unwrap();
    let regenerated: Value = client
        .post(format!("{base}/v1/conversations/{branch_id}/regenerate"))
        .json(&json!({"from_message_id": branch_message_id, "model": "gemini-web-flash"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let regenerated_id = regenerated["conversation_id"].as_str().unwrap();
    let completion = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "conversation_id": regenerated_id,
            "messages": [{"role": "user", "content": "regenerate from selected state"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(completion.status(), 200);
    assert_eq!(store.get_history(&conversation.id).await.unwrap().len(), 4);
    assert_eq!(store.get_history(branch_id).await.unwrap().len(), 2);
    assert_eq!(store.get_history(regenerated_id).await.unwrap().len(), 4);
    server.abort();
}

#[tokio::test]
async fn rejected_upstream_ids_replay_history_and_mark_degraded() {
    let (base, server, adapter, _store) = test_server().await;
    let client = reqwest::Client::new();

    let first: Value = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "first"}]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let conversation_id = first["gemini_metadata"]["conversation_id"]
        .as_str()
        .unwrap();
    adapter.reject_next_continuation();

    let response = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "conversation_id": conversation_id,
            "messages": [{"role": "user", "content": "second"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-gemini-bridge-continuity"], "degraded");

    let requests = adapter.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].metadata, BTreeMap::new());
    assert_eq!(requests[2].messages.len(), 3);
    server.abort();
}
