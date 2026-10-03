//! Database migrations and schema definitions for `conversation-store`.

use crate::error::ConversationStoreError;
use rusqlite::{Connection, params};

/// Original schema shipped by the crate.
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

/// V2 deterministically moves duplicate legacy sequence values past the
/// conversation's existing maximum, preserving every message and ordering
/// duplicate ties by creation time and ID. It then makes uniqueness a database
/// invariant. Deleting a branched-from conversation also clears both parent
/// references; `parent_message_id` deliberately is not a message foreign key
/// because branch history is copied with new message IDs.
const MIGRATION_V2: &str = r#"
WITH ranked AS (
    SELECT id, conversation_id, sequence_number, created_at,
           ROW_NUMBER() OVER (
               PARTITION BY conversation_id, sequence_number
               ORDER BY created_at ASC, id ASC
           ) AS duplicate_rank,
           MAX(sequence_number) OVER (PARTITION BY conversation_id) AS max_sequence
    FROM messages
), moved AS (
    SELECT id,
           max_sequence + ROW_NUMBER() OVER (
               PARTITION BY conversation_id
               ORDER BY sequence_number ASC, created_at ASC, id ASC
           ) AS new_sequence
    FROM ranked
    WHERE duplicate_rank > 1
)
UPDATE messages
   SET sequence_number = (SELECT new_sequence FROM moved WHERE moved.id = messages.id)
 WHERE id IN (SELECT id FROM moved);

CREATE UNIQUE INDEX IF NOT EXISTS uq_messages_conversation_sequence
    ON messages(conversation_id, sequence_number);

CREATE TRIGGER IF NOT EXISTS clear_deleted_parent_message_id
BEFORE DELETE ON conversations
FOR EACH ROW
BEGIN
    UPDATE conversations SET parent_message_id = NULL WHERE parent_id = OLD.id;
END;
"#;

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn apply_migrations(conn: &mut Connection) -> Result<(), ConversationStoreError> {
    // `foreign_keys` is connection-local and cannot be toggled in a transaction.
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    let tx = conn.transaction()?;
    let now = now_unix();

    tx.execute_batch(MIGRATION_V1)?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, applied_at) VALUES (1, ?1)",
        params![now],
    )?;

    let has_v2: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 2)",
        [],
        |row| row.get(0),
    )?;
    if !has_v2 {
        tx.execute_batch(MIGRATION_V2)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (2, ?1)",
            params![now],
        )?;
    }

    tx.commit()?;

    let foreign_keys_enabled: bool = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if !foreign_keys_enabled {
        return Err(ConversationStoreError::Migration(
            "SQLite foreign key enforcement could not be enabled".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_v2_migration_rolls_back_sequence_updates_and_version_marker() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        connection.execute_batch(MIGRATION_V1).unwrap();
        connection
            .execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (1, 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO conversations (id, created_at, updated_at) VALUES ('rollback-conv', 1, 1)",
                [],
            )
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO messages (id, conversation_id, role, content_json, sequence_number, created_at)
                 VALUES ('first', 'rollback-conv', 'user', 'null', 1, 1),
                        ('second', 'rollback-conv', 'assistant', 'null', 1, 2);
                 CREATE TABLE uq_messages_conversation_sequence (sentinel INTEGER);",
            )
            .unwrap();

        assert!(apply_migrations(&mut connection).is_err());

        let sequences: Vec<i64> = {
            let mut statement = connection
                .prepare("SELECT sequence_number FROM messages ORDER BY id")
                .unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            sequences,
            [1, 1],
            "failed migration must roll back row rewrites"
        );
        let v2_applied: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 2)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!v2_applied, "failed migration must not record its version");
    }
}
