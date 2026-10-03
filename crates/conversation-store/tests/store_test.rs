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

#[cfg(unix)]
#[test]
fn opening_database_secures_parent_database_and_sqlite_sidecars() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = dir.path().join("conversations.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
    connection
        .execute_batch("CREATE TABLE permission_probe (value INTEGER); INSERT INTO permission_probe VALUES (1);")
        .unwrap();

    let wal_path = std::path::PathBuf::from(format!("{}-wal", path.display()));
    let shm_path = std::path::PathBuf::from(format!("{}-shm", path.display()));
    assert!(wal_path.exists());
    assert!(shm_path.exists());
    for file in [&path, &wal_path, &shm_path] {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    let _store = SqliteConversationStore::open(&path).unwrap();

    assert_eq!(
        std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
        0o755,
        "open must not change caller-owned parent permissions"
    );
    for file in [&path, &wal_path, &shm_path] {
        assert_eq!(
            std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600,
            "{} must be private",
            file.display()
        );
    }

    let nested_parent = dir.path().join("new-data");
    let nested_db = nested_parent.join("conversations.sqlite");
    let _nested_store = SqliteConversationStore::open(&nested_db).unwrap();
    assert_eq!(
        std::fs::metadata(nested_parent)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "new database parent directories must be private"
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
// ---------------------------------------------------------------------------
// Durability & concurrency regression coverage
// ---------------------------------------------------------------------------

/// A complete logical turn: request messages, the assistant response, and the
/// upstream identifiers returned for the turn. Mirrors what the HTTP layer
/// builds today in `persist_turn`.
fn make_turn(
    conversation_id: &str,
    request_texts: &[&str],
    response_text: &str,
    upstream_conversation_id: Option<&str>,
    upstream_response_id: Option<&str>,
    upstream_candidate_id: Option<&str>,
) -> gemini_bridge_conversation_store::Turn {
    gemini_bridge_conversation_store::Turn {
        conversation_id: conversation_id.to_owned(),
        request_messages: request_texts
            .iter()
            .map(|text| gemini_bridge_conversation_store::StoredMessage {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_owned(),
                parent_message_id: None,
                role: "user".to_owned(),
                content_json: serde_json::to_string(text).unwrap(),
                sequence_number: 0, // allocated by the store
                created_at: now(),
                upstream_conversation_id: None,
                upstream_response_id: None,
                upstream_candidate_id: None,
            })
            .collect(),
        response: gemini_bridge_conversation_store::StoredMessage {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_owned(),
            parent_message_id: None,
            role: "assistant".to_owned(),
            content_json: serde_json::to_string(response_text).unwrap(),
            sequence_number: 0, // allocated by the store
            created_at: now(),
            upstream_conversation_id: upstream_conversation_id.map(str::to_owned),
            upstream_response_id: upstream_response_id.map(str::to_owned),
            upstream_candidate_id: upstream_candidate_id.map(str::to_owned),
        },
    }
}

#[tokio::test]
async fn persist_turn_rolls_back_all_messages_and_upstream_ids_on_failure() {
    let store = SqliteConversationStore::in_memory().unwrap();
    let conversation = store.create_conversation(None).await.unwrap();

    // Pre-existing state that must survive a failed turn.
    store
        .update_upstream_ids(
            &conversation.id,
            Some("old-conv"),
            Some("old-resp"),
            Some("old-candidate"),
        )
        .await
        .unwrap();

    let mut turn = make_turn(
        &conversation.id,
        &["hello"],
        "hi there",
        Some("new-conv"),
        Some("new-resp"),
        Some("new-candidate"),
    );
    turn.response.id = "forced-duplicate".to_owned();
    store.persist_turn(turn).await.unwrap();

    // The response hits a duplicate primary key only after the request
    // message has been inserted, so rollback must remove that partial write.
    let mut failing = make_turn(
        &conversation.id,
        &["second"],
        "second response",
        Some("newer-conv"),
        Some("newer-resp"),
        Some("newer-candidate"),
    );
    failing.response.id = "forced-duplicate".to_owned();
    assert!(store.persist_turn(failing).await.is_err());

    let history = store.get_history(&conversation.id).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].content_json, "\"hello\"");
    assert_eq!(history[1].content_json, "\"hi there\"");

    // The conversation-level upstream mapping must not reflect the failed turn.
    let fetched = store.get_conversation(&conversation.id).await.unwrap();
    assert_eq!(
        fetched.upstream_conversation_id.as_deref(),
        Some("new-conv")
    );
    assert_eq!(fetched.upstream_response_id.as_deref(), Some("new-resp"));
    assert_eq!(
        fetched.upstream_candidate_id.as_deref(),
        Some("new-candidate")
    );
    // Sequences are contiguous: no gap from the rolled-back turn.
    assert_eq!(
        history
            .iter()
            .map(|m| m.sequence_number)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[tokio::test]
async fn persist_turn_allocates_unique_deterministic_sequences_under_concurrency() {
    let store = SqliteConversationStore::in_memory().unwrap();
    let conversation = store.create_conversation(None).await.unwrap();

    let mut handles = Vec::new();
    for i in 0..16 {
        let store = store.clone();
        let conversation_id = conversation.id.clone();
        handles.push(tokio::spawn(async move {
            store
                .persist_turn(make_turn(
                    &conversation_id,
                    &[&format!("request-{i}")],
                    &format!("response-{i}"),
                    Some(&format!("conv-{i}")),
                    Some(&format!("resp-{i}")),
                    Some(&format!("cand-{i}")),
                ))
                .await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    let history = store.get_history(&conversation.id).await.unwrap();
    assert_eq!(history.len(), 32);
    let mut sequences = history
        .iter()
        .map(|m| m.sequence_number)
        .collect::<Vec<_>>();
    assert_eq!(sequences.len(), 32);
    sequences.sort_unstable();
    // Exactly one message per sequence, starting at 1, no gaps, no duplicates.
    assert_eq!(sequences, (1..=32).collect::<Vec<_>>());

    // The last writer wins the conversation-level upstream mapping, and every
    // writer's response message carries the upstream identifiers it reported.
    for i in 0..16 {
        let conv = format!("conv-{i}");
        let resp = format!("resp-{i}");
        let cand = format!("cand-{i}");
        assert!(
            history.iter().any(
                |m| m.upstream_conversation_id.as_deref() == Some(conv.as_str())
                    && m.upstream_response_id.as_deref() == Some(resp.as_str())
                    && m.upstream_candidate_id.as_deref() == Some(cand.as_str())
            ),
            "turn {i} upstream ids missing from history"
        );
    }

    for pair in history.chunks_exact(2) {
        assert_eq!(pair[0].role, "user");
        assert_eq!(pair[1].role, "assistant");
        assert_eq!(pair[1].sequence_number, pair[0].sequence_number + 1);
        let request_index = pair[0]
            .content_json
            .trim_matches('"')
            .strip_prefix("request-")
            .unwrap();
        assert_eq!(
            pair[1].content_json,
            format!("\"response-{request_index}\"")
        );
    }
}

#[tokio::test]
async fn history_pagination_is_bounded_and_offset_consistent() {
    let store = SqliteConversationStore::in_memory().unwrap();
    let conversation = store.create_conversation(None).await.unwrap();
    for i in 0..10 {
        store
            .append_message(make_message(&conversation.id, i + 1, "user"))
            .await
            .unwrap();
    }

    let page1 = store
        .get_history_page(&conversation.id, 4, 0)
        .await
        .unwrap();
    let page2 = store
        .get_history_page(&conversation.id, 4, 4)
        .await
        .unwrap();
    let page3 = store
        .get_history_page(&conversation.id, 4, 8)
        .await
        .unwrap();
    let past_end = store
        .get_history_page(&conversation.id, 4, 20)
        .await
        .unwrap();

    assert_eq!(page1.len(), 4);
    assert_eq!(page2.len(), 4);
    assert_eq!(page3.len(), 2);
    assert!(past_end.is_empty());

    let full = store.get_history(&conversation.id).await.unwrap();
    assert_eq!(full.len(), 10);
    assert_eq!(page1[0].id, full[0].id);
    assert_eq!(page1[3].id, full[3].id);
    assert_eq!(page2[0].id, full[4].id);
    assert_eq!(page3[1].id, full[9].id);

    assert!(matches!(
        store
            .get_history_page(&conversation.id, usize::MAX, 0)
            .await,
        Err(ConversationStoreError::PaginationOverflow("limit", _))
    ));
    assert!(matches!(
        store
            .get_history_page(&conversation.id, 4, usize::MAX)
            .await,
        Err(ConversationStoreError::PaginationOverflow("offset", _))
    ));
}

#[tokio::test]
async fn delete_conversation_cascades_messages_but_sets_branch_parent_null() {
    let store = SqliteConversationStore::in_memory().unwrap();
    let parent = store.create_conversation(None).await.unwrap();
    for i in 0..3 {
        store
            .append_message(make_message(&parent.id, i + 1, "user"))
            .await
            .unwrap();
    }

    // Give the branch a concrete anchor message instead of a random UUID so
    // the relationship is meaningful before we delete the parent.
    let anchor = make_message(&parent.id, 4, "user");
    store.append_message(anchor.clone()).await.unwrap();
    let branch = store
        .branch_from(&parent.id, &anchor.id, None)
        .await
        .unwrap();
    assert_eq!(
        branch.parent_message_id.as_deref(),
        Some(anchor.id.as_str())
    );

    store.delete_conversation(&parent.id).await.unwrap();

    // CASCADE: the parent conversation and all of its own messages are gone.
    assert!(matches!(
        store.get_conversation(&parent.id).await,
        Err(ConversationStoreError::NotFound(_))
    ));
    assert!(store.get_history(&parent.id).await.unwrap().is_empty());

    // SET NULL: the branch survives and loses its parent linkage, and its
    // copied ancestor history is untouched by the parent's deletion.
    let orphan = store.get_conversation(&branch.id).await.unwrap();
    assert_eq!(orphan.parent_id, None);
    assert_eq!(orphan.parent_message_id, None);
    assert_eq!(store.get_history(&branch.id).await.unwrap().len(), 4);
}

#[tokio::test]
async fn v1_database_migrates_to_v2_without_data_loss_and_is_idempotent() {
    let file = NamedTempFile::new().unwrap();
    let path = file.path().to_owned();

    // Build a V1 database by hand, exactly as `MIGRATION_V1` defined it.
    {
        use rusqlite::Connection;
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version     INTEGER PRIMARY KEY,
                applied_at  INTEGER NOT NULL
            );
            INSERT INTO schema_migrations (version, applied_at) VALUES (1, 1);

            CREATE TABLE conversations (
                id                       TEXT PRIMARY KEY,
                title                    TEXT,
                parent_id                TEXT,
                parent_message_id        TEXT,
                upstream_conversation_id TEXT,
                upstream_response_id     TEXT,
                upstream_candidate_id    TEXT,
                created_at               INTEGER NOT NULL,
                updated_at               INTEGER NOT NULL,
                FOREIGN KEY(parent_id) REFERENCES conversations(id) ON DELETE SET NULL
            );

            CREATE TABLE messages (
                id                       TEXT PRIMARY KEY,
                conversation_id          TEXT NOT NULL,
                parent_message_id        TEXT,
                role                     TEXT NOT NULL,
                content_json             TEXT NOT NULL,
                sequence_number          INTEGER NOT NULL,
                created_at               INTEGER NOT NULL,
                upstream_conversation_id TEXT,
                upstream_response_id     TEXT,
                upstream_candidate_id    TEXT,
                FOREIGN KEY(conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
            );

            CREATE INDEX idx_messages_conv_seq
                ON messages(conversation_id, sequence_number ASC);
            CREATE INDEX idx_conversations_updated
                ON conversations(updated_at DESC);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (
                id, title, parent_id, parent_message_id,
                upstream_conversation_id, upstream_response_id, upstream_candidate_id,
                created_at, updated_at
            ) VALUES ('v1-conv', 'legacy', NULL, NULL, 'u-conv', 'u-resp', 'u-cand', 100, 200)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (
                id, conversation_id, parent_message_id, role, content_json,
                sequence_number, created_at, upstream_conversation_id,
                upstream_response_id, upstream_candidate_id
            ) VALUES ('v1-msg-1', 'v1-conv', NULL, 'user', '\"hello from v1\"', 1, 100,
                      'u-conv', 'u-resp', 'u-cand'),
                     ('v1-msg-2', 'v1-conv', 'v1-msg-1', 'assistant', '\"legacy reply\"', 2, 150,
                      NULL, NULL, NULL),
                     ('v1-msg-duplicate-a', 'v1-conv', NULL, 'user', '\"duplicate a\"', 1, 100,
                      NULL, NULL, NULL),
                     ('v1-msg-duplicate-b', 'v1-conv', NULL, 'user', '\"duplicate b\"', 1, 100,
                      NULL, NULL, NULL),
                     ('v1-msg-high', 'v1-conv', NULL, 'tool', '\"high\"', 10, 300,
                      NULL, NULL, NULL)",
            [],
        )
        .unwrap();
    }

    // First open runs V1 -> V2.
    let store = SqliteConversationStore::open(&path).unwrap();
    let conversation = store.get_conversation("v1-conv").await.unwrap();
    assert_eq!(conversation.title.as_deref(), Some("legacy"));
    assert_eq!(
        conversation.upstream_conversation_id.as_deref(),
        Some("u-conv")
    );
    assert_eq!(conversation.upstream_response_id.as_deref(), Some("u-resp"));
    assert_eq!(
        conversation.upstream_candidate_id.as_deref(),
        Some("u-cand")
    );

    let history = store.get_history("v1-conv").await.unwrap();
    assert_eq!(history.len(), 5);
    assert_eq!(
        history
            .iter()
            .map(|message| (message.id.as_str(), message.sequence_number))
            .collect::<Vec<_>>(),
        [
            ("v1-msg-1", 1),
            ("v1-msg-2", 2),
            ("v1-msg-high", 10),
            ("v1-msg-duplicate-a", 11),
            ("v1-msg-duplicate-b", 12),
        ]
    );
    assert_eq!(history[0].content_json, "\"hello from v1\"");
    assert_eq!(history[1].parent_message_id.as_deref(), Some("v1-msg-1"));
    assert_eq!(
        history[0].upstream_conversation_id.as_deref(),
        Some("u-conv")
    );
    assert_eq!(history[0].upstream_response_id.as_deref(), Some("u-resp"));
    assert_eq!(history[0].upstream_candidate_id.as_deref(), Some("u-cand"));

    // Legacy rows must be protected by the new uniqueness invariant: a
    // message claiming a sequence the legacy data already owns is rejected.
    let conflict = store
        .append_message(make_message("v1-conv", 1, "user"))
        .await;
    assert!(matches!(
        conflict,
        Err(ConversationStoreError::SequenceConflict(1, _))
    ));

    // Idempotency: reopening must not re-run or corrupt the migration.
    drop(store);
    let reopened = SqliteConversationStore::open(&path).unwrap();
    let history_again = reopened.get_history("v1-conv").await.unwrap();
    assert_eq!(history_again, history);
    let conversation_again = reopened.get_conversation("v1-conv").await.unwrap();
    assert_eq!(
        conversation_again.upstream_conversation_id.as_deref(),
        Some("u-conv")
    );

    // Fresh writes coexist with legacy data, continuing after the old max.
    let turn = make_turn(
        "v1-conv",
        &["post-migration"],
        "post-migration reply",
        Some("u-conv-2"),
        Some("u-resp-2"),
        Some("u-cand-2"),
    );
    reopened.persist_turn(turn).await.unwrap();
    let final_history = reopened.get_history("v1-conv").await.unwrap();
    assert_eq!(final_history.len(), 7);
    assert_eq!(
        final_history
            .iter()
            .map(|m| m.sequence_number)
            .collect::<Vec<_>>(),
        [1, 2, 10, 11, 12, 13, 14]
    );
    assert_eq!(
        reopened
            .get_conversation("v1-conv")
            .await
            .unwrap()
            .upstream_response_id
            .as_deref(),
        Some("u-resp-2")
    );
}

#[tokio::test]
async fn persist_turn_rejects_missing_conversation_without_writing() {
    let store = SqliteConversationStore::in_memory().unwrap();
    let turn = make_turn("missing", &["hi"], "yo", None, None, None);
    assert!(matches!(
        store.persist_turn(turn).await,
        Err(ConversationStoreError::NotFound(_))
    ));
}
