//! Database migrations and schema definitions for `conversation-store`.

use crate::error::ConversationStoreError;
use rusqlite::Connection;

pub(crate) const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version     INTEGER PRIMARY KEY,
    applied_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS conversations (
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

CREATE TABLE IF NOT EXISTS messages (
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

CREATE INDEX IF NOT EXISTS idx_messages_conv_seq
    ON messages(conversation_id, sequence_number ASC);

CREATE INDEX IF NOT EXISTS idx_conversations_updated
    ON conversations(updated_at DESC);
"#;

pub(crate) fn apply_migrations(conn: &mut Connection) -> Result<(), ConversationStoreError> {
    // Enable foreign keys for this connection.
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    let tx = conn.transaction()?;

    tx.execute_batch(MIGRATION_V1)?;

    // Record migration v1 if not already applied.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, applied_at) VALUES (1, ?1)",
        rusqlite::params![now],
    )?;

    tx.commit()?;
    Ok(())
}
