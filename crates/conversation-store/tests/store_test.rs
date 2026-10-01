//! Integration tests for the SQLite conversation persistence contract.

use gemini_bridge_conversation_store::{
    ConversationStore, ConversationStoreError, SqliteConversationStore, StoredMessage,
};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn make_message(conversation_id: &str, seq: i64, role: &str) -> StoredMessage {
    StoredMessage {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.to_owned(),
        parent_message_id: None,
        role: role.to_owned(),
        content_json: format!("\"msg-{seq}\""),
        sequence_number: seq,
        created_at: now(),
        upstream_conversation_id: None,
        upstream_response_id: None,
        upstream_candidate_id: None,
    }
}

#[tokio::test]
async fn fresh_database_initializes_and_migration_is_idempotent() {
    let file = NamedTempFile::new().unwrap();
    let path = file.path();

    let first = SqliteConversationStore::open(path).expect("initialize fresh database");
    let conversation = first.create_conversation(None).await.unwrap();
    drop(first);

    // Reopen the same database: the migration must not recreate or corrupt the schema.
    let reopened = SqliteConversationStore::open(path).expect("reopen migrated database");
    assert_eq!(
        reopened
            .get_conversation(&conversation.id)
            .await
            .unwrap()
            .id,
        conversation.id
    );
}

#[tokio::test]
async fn persisted_history_is_ordered_and_round_trips_upstream_ids_after_reopen() {
    let file = NamedTempFile::new().unwrap();
    let path = file.path().to_owned();
    let store = SqliteConversationStore::open(&path).unwrap();
    let conversation = store
        .create_conversation(Some("history".into()))
        .await
        .unwrap();

    let mut third = make_message(&conversation.id, 3, "assistant");
    third.upstream_conversation_id = Some("gcid-3".into());
    third.upstream_response_id = Some("rid-3".into());
    third.upstream_candidate_id = Some("cid-3".into());
    let first = make_message(&conversation.id, 1, "user");
    let second = make_message(&conversation.id, 2, "assistant");

    // Insert out of order; history is defined by sequence, not insertion time.
    store.append_message(third.clone()).await.unwrap();
    store.append_message(first.clone()).await.unwrap();
    store.append_message(second.clone()).await.unwrap();
    drop(store);

    let reopened = SqliteConversationStore::open(&path).unwrap();
    let fetched_conversation = reopened.get_conversation(&conversation.id).await.unwrap();
    assert_eq!(fetched_conversation.title.as_deref(), Some("history"));

    let history = reopened.get_history(&conversation.id).await.unwrap();
    assert_eq!(
        history.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        [first.id.as_str(), second.id.as_str(), third.id.as_str()]
    );
    assert_eq!(
        history[2].upstream_conversation_id.as_deref(),
        Some("gcid-3")
    );
    assert_eq!(history[2].upstream_response_id.as_deref(), Some("rid-3"));
    assert_eq!(history[2].upstream_candidate_id.as_deref(), Some("cid-3"));

    // The conversation-level mapping tracks the latest successful turn.
    assert_eq!(
        fetched_conversation.upstream_conversation_id.as_deref(),
        Some("gcid-3")
    );
    assert_eq!(
        fetched_conversation.upstream_response_id.as_deref(),
        Some("rid-3")
    );
    assert_eq!(
        fetched_conversation.upstream_candidate_id.as_deref(),
        Some("cid-3")
    );
}

#[tokio::test]
async fn failed_append_rolls_back_message_and_conversation_upstream_ids() {
    let store = SqliteConversationStore::in_memory().unwrap();
    let conversation = store.create_conversation(None).await.unwrap();
    store
        .update_upstream_ids(
            &conversation.id,
            Some("old-conv"),
            Some("old-resp"),
            Some("old-candidate"),
        )
        .await
        .unwrap();

    let mut original = make_message(&conversation.id, 1, "user");
    original.id = "duplicate-id".into();
    store.append_message(original.clone()).await.unwrap();

    // A duplicate primary key forces the message insert to fail. New IDs must
    // not leak into the conversation metadata when that transaction rolls back.
    let mut duplicate = make_message(&conversation.id, 2, "assistant");
    duplicate.id = original.id;
    duplicate.upstream_conversation_id = Some("new-conv".into());
    duplicate.upstream_response_id = Some("new-resp".into());
    duplicate.upstream_candidate_id = Some("new-candidate".into());
    assert!(store.append_message(duplicate).await.is_err());

    let fetched = store.get_conversation(&conversation.id).await.unwrap();
    assert_eq!(
        fetched.upstream_conversation_id.as_deref(),
        Some("old-conv")
    );
    assert_eq!(fetched.upstream_response_id.as_deref(), Some("old-resp"));
    assert_eq!(
        fetched.upstream_candidate_id.as_deref(),
        Some("old-candidate")
    );
    let history = store.get_history(&conversation.id).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, "duplicate-id");
}

#[tokio::test]
async fn get_conversation_reports_not_found() {
    let store = SqliteConversationStore::in_memory().unwrap();
    assert!(matches!(
        store.get_conversation("missing").await,
        Err(ConversationStoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn append_rejects_a_message_for_a_missing_conversation() {
    let store = SqliteConversationStore::in_memory().unwrap();
    let result = store
        .append_message(make_message("missing", 1, "user"))
        .await;
    assert!(matches!(result, Err(ConversationStoreError::NotFound(_))));
}
#[tokio::test]
async fn branch_copies_only_selected_ancestor_and_uses_its_upstream_snapshot() {
    let store = SqliteConversationStore::in_memory().unwrap();
    let source = store
        .create_conversation(Some("original".to_owned()))
        .await
        .unwrap();

    for sequence_number in 1..=5 {
        let mut message = make_message(
            &source.id,
            sequence_number,
            if sequence_number % 2 == 1 {
                "user"
            } else {
                "assistant"
            },
        );
        message.id = format!("source-{sequence_number}");
        if sequence_number == 3 {
            message.upstream_conversation_id = Some("conversation-at-3".to_owned());
            message.upstream_response_id = Some("response-at-3".to_owned());
            message.upstream_candidate_id = Some("candidate-at-3".to_owned());
        }
        if sequence_number == 5 {
            message.upstream_conversation_id = Some("conversation-at-5".to_owned());
            message.upstream_response_id = Some("response-at-5".to_owned());
            message.upstream_candidate_id = Some("candidate-at-5".to_owned());
        }
        store.append_message(message).await.unwrap();
    }

    let branch = store
        .branch_from(&source.id, "source-3", Some("alternate".to_owned()))
        .await
        .unwrap();
    let branch_history = store.get_history(&branch.id).await.unwrap();
    let source_history = store.get_history(&source.id).await.unwrap();

    assert_eq!(branch.parent_id.as_deref(), Some(source.id.as_str()));
    assert_eq!(branch.parent_message_id.as_deref(), Some("source-3"));
    assert_eq!(branch.title.as_deref(), Some("alternate"));
    assert_eq!(
        branch_history
            .iter()
            .map(|message| message.sequence_number)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(
        branch_history[2].content_json,
        source_history[2].content_json
    );
    assert_eq!(
        branch.upstream_conversation_id.as_deref(),
        Some("conversation-at-3")
    );
    assert_eq!(
        branch.upstream_response_id.as_deref(),
        Some("response-at-3")
    );
    assert_eq!(
        branch.upstream_candidate_id.as_deref(),
        Some("candidate-at-3")
    );
    assert_eq!(source_history.len(), 5);
}
