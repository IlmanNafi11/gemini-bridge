use crate::error::ConversationStoreError;
use crate::models::{Conversation, StoredMessage, Turn};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Embedded SQLite implementation of `ConversationStore`.
///
/// Wraps a single SQLite connection protected by a `std::sync::Mutex` so it is
/// safely shared across async tasks via `tokio::task::spawn_blocking`. A panic
/// on one task poisons the mutex rather than being silently swallowed: later
/// operations return [`ConversationStoreError::LockPoisoned`] instead of
/// panicking, so one bad turn cannot take the whole bridge down.
#[derive(Clone)]
pub struct SqliteConversationStore {
    conn: Arc<Mutex<Connection>>,
    #[allow(dead_code)]
    db_path: Option<PathBuf>,
}

/// Result of locking the shared connection.
type ConnGuard<'a> = std::sync::MutexGuard<'a, Connection>;

fn lock_connection(conn: &Mutex<Connection>) -> Result<ConnGuard<'_>, ConversationStoreError> {
    conn.lock()
        .map_err(|poisoned| ConversationStoreError::LockPoisoned(poisoned.to_string()))
}

#[cfg(unix)]
fn secure_database_paths(path: &Path) -> Result<(), ConversationStoreError> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let ancestors = parent.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(ancestors)
        .map_err(|_| ConversationStoreError::Migration("failed to create db dir".to_owned()))?;
    match std::fs::DirBuilder::new().mode(0o700).create(parent) {
        Ok(()) => std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| ConversationStoreError::Migration("failed to secure db dir".to_owned()))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            return Err(ConversationStoreError::Migration(
                "failed to create db dir".to_owned(),
            ));
        }
    }
    // SQLite inherits the database mode for newly-created journal files. Repair
    // pre-existing main database and sidecar files before opening SQLite.
    secure_database_files(path)?;
    // Create a missing main database privately before SQLite opens it.
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|_| ConversationStoreError::Migration("failed to create db file".to_owned()))?;
    secure_database_files(path)
}

#[cfg(unix)]
fn secure_database_files(path: &Path) -> Result<(), ConversationStoreError> {
    use std::os::unix::fs::PermissionsExt;

    for file in database_and_sidecars(path) {
        if file.exists() {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).map_err(
                |_| ConversationStoreError::Migration("failed to secure db file".to_owned()),
            )?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_database_files(_path: &Path) -> Result<(), ConversationStoreError> {
    Ok(())
}

#[cfg(not(unix))]
fn secure_database_paths(path: &Path) -> Result<(), ConversationStoreError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|_| ConversationStoreError::Migration("failed to create db dir".to_owned()))?;
    Ok(())
}

fn database_and_sidecars(path: &Path) -> [PathBuf; 4] {
    let mut journal = path.as_os_str().to_owned();
    journal.push("-journal");
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let mut shm = path.as_os_str().to_owned();
    shm.push("-shm");
    [path.to_path_buf(), journal.into(), wal.into(), shm.into()]
}

impl SqliteConversationStore {
    /// Open or create a conversation store at the specified filesystem path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ConversationStoreError> {
        let path_ref = path.as_ref();
        secure_database_paths(path_ref)?;
        let mut conn = Connection::open(path_ref)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        crate::migrations::apply_migrations(&mut conn)?;
        secure_database_files(path_ref)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: Some(path_ref.to_path_buf()),
        })
    }

    /// Open an in-memory conversation store (useful for tests).
    pub fn in_memory() -> Result<Self, ConversationStoreError> {
        let mut conn = Connection::open_in_memory()?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        crate::migrations::apply_migrations(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: None,
        })
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn conversation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: row.get(0)?,
        title: row.get(1)?,
        parent_id: row.get(2)?,
        parent_message_id: row.get(3)?,
        upstream_conversation_id: row.get(4)?,
        upstream_response_id: row.get(5)?,
        upstream_candidate_id: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        parent_message_id: row.get(2)?,
        role: row.get(3)?,
        content_json: row.get(4)?,
        sequence_number: row.get(5)?,
        created_at: row.get(6)?,
        upstream_conversation_id: row.get(7)?,
        upstream_response_id: row.get(8)?,
        upstream_candidate_id: row.get(9)?,
    })
}

const MESSAGE_COLUMNS: &str = "id, conversation_id, parent_message_id, role, content_json, \
     sequence_number, created_at, upstream_conversation_id, upstream_response_id, \
     upstream_candidate_id";

/// Distinguish duplicate-sequence and duplicate-message-id constraint hits.
fn classify_constraint_error(
    error: rusqlite::Error,
    conversation_id: &str,
    sequence_number: i64,
    message_id: &str,
) -> ConversationStoreError {
    if matches!(&error, rusqlite::Error::SqliteFailure(_, Some(msg))
        if msg.contains("messages.conversation_id, messages.sequence_number"))
    {
        ConversationStoreError::SequenceConflict(sequence_number, conversation_id.to_owned())
    } else if matches!(&error, rusqlite::Error::SqliteFailure(_, Some(msg))
        if msg.contains("UNIQUE constraint failed: messages.id"))
    {
        ConversationStoreError::DuplicateMessage(conversation_id.to_owned(), message_id.to_owned())
    } else {
        error.into()
    }
}

/// Persist one message row and keep the conversation's updated_at and upstream
/// mapping current. Must be called from inside an open transaction.
fn insert_message_tx(
    tx: &Transaction<'_>,
    message: &StoredMessage,
    touch_upstream: bool,
) -> Result<(), ConversationStoreError> {
    let now = now_unix();
    tx.execute(
        "INSERT INTO messages (
            id, conversation_id, parent_message_id, role, content_json,
            sequence_number, created_at, upstream_conversation_id,
            upstream_response_id, upstream_candidate_id
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            message.id,
            message.conversation_id,
            message.parent_message_id,
            message.role,
            message.content_json,
            message.sequence_number,
            message.created_at,
            message.upstream_conversation_id,
            message.upstream_response_id,
            message.upstream_candidate_id,
        ],
    )
    .map_err(|error| {
        classify_constraint_error(
            error,
            &message.conversation_id,
            message.sequence_number,
            &message.id,
        )
    })?;

    if touch_upstream {
        tx.execute(
            "UPDATE conversations
               SET updated_at = ?1,
                   upstream_conversation_id = COALESCE(?2, upstream_conversation_id),
                   upstream_response_id = COALESCE(?3, upstream_response_id),
                   upstream_candidate_id = COALESCE(?4, upstream_candidate_id)
             WHERE id = ?5",
            params![
                now,
                message.upstream_conversation_id,
                message.upstream_response_id,
                message.upstream_candidate_id,
                message.conversation_id
            ],
        )?;
    } else {
        tx.execute(
            "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
            params![now, message.conversation_id],
        )?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl crate::ConversationStore for SqliteConversationStore {
    async fn create_conversation(
        &self,
        title: Option<String>,
    ) -> Result<Conversation, ConversationStoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = lock_connection(&conn)?;
            let id = Uuid::new_v4().to_string();
            let now = now_unix();

            conn.execute(
                "INSERT INTO conversations (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
                params![id, title, now, now],
            )?;

            Ok(Conversation {
                id,
                title,
                parent_id: None,
                parent_message_id: None,
                upstream_conversation_id: None,
                upstream_response_id: None,
                upstream_candidate_id: None,
                created_at: now,
                updated_at: now,
            })
        })
        .await
        .map_err(join_error)?
    }

    async fn get_conversation(&self, id: &str) -> Result<Conversation, ConversationStoreError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = lock_connection(&conn)?;
            let mut stmt = conn.prepare(
                "SELECT id, title, parent_id, parent_message_id, upstream_conversation_id, upstream_response_id, upstream_candidate_id, created_at, updated_at
                 FROM conversations WHERE id = ?1",
            )?;

            let conv = stmt
                .query_row(params![id], conversation_from_row)
                .optional()?;

            conv.ok_or_else(|| ConversationStoreError::NotFound(id))
        })
        .await
        .map_err(join_error)?
    }

    async fn update_upstream_ids(
        &self,
        id: &str,
        upstream_conversation_id: Option<&str>,
        upstream_response_id: Option<&str>,
        upstream_candidate_id: Option<&str>,
    ) -> Result<(), ConversationStoreError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        let u_cid = upstream_conversation_id.map(str::to_owned);
        let u_rid = upstream_response_id.map(str::to_owned);
        let u_cand_id = upstream_candidate_id.map(str::to_owned);

        tokio::task::spawn_blocking(move || {
            let conn = lock_connection(&conn)?;
            let now = now_unix();
            let affected = conn.execute(
                "UPDATE conversations
                 SET upstream_conversation_id = ?1,
                     upstream_response_id = ?2,
                     upstream_candidate_id = ?3,
                     updated_at = ?4
                 WHERE id = ?5",
                params![u_cid, u_rid, u_cand_id, now, id],
            )?;

            if affected == 0 {
                return Err(ConversationStoreError::NotFound(id));
            }
            Ok(())
        })
        .await
        .map_err(join_error)?
    }

    async fn append_message(&self, message: StoredMessage) -> Result<(), ConversationStoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = lock_connection(&conn)?;
            let tx = conn.transaction()?;
            // The supplied sequence is caller-owned here (legacy single-message
            // contract); enforce a usable positive value before writing.
            if message.sequence_number <= 0 {
                return Err(ConversationStoreError::InvalidSequence(
                    message.sequence_number,
                ));
            }

            // Verify conversation exists.
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM conversations WHERE id = ?1",
                    params![message.conversation_id],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);

            if !exists {
                return Err(ConversationStoreError::NotFound(message.conversation_id));
            }

            let touch_upstream = message.upstream_conversation_id.is_some()
                || message.upstream_response_id.is_some()
                || message.upstream_candidate_id.is_some();
            insert_message_tx(&tx, &message, touch_upstream)?;

            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(join_error)?
    }

    async fn get_history(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<StoredMessage>, ConversationStoreError> {
        let conn = self.conn.clone();
        let conversation_id = conversation_id.to_owned();

        tokio::task::spawn_blocking(move || {
            let conn = lock_connection(&conn)?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages
                 WHERE conversation_id = ?1
                 ORDER BY sequence_number ASC"
            ))?;

            let rows = stmt.query_map(params![conversation_id], message_from_row)?;

            let mut messages = Vec::new();
            for msg in rows {
                messages.push(msg?);
            }
            Ok(messages)
        })
        .await
        .map_err(join_error)?
    }

    async fn get_history_page(
        &self,
        conversation_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredMessage>, ConversationStoreError> {
        let limit = i64::try_from(limit)
            .map_err(|_| ConversationStoreError::PaginationOverflow("limit", limit))?;
        let offset = i64::try_from(offset)
            .map_err(|_| ConversationStoreError::PaginationOverflow("offset", offset))?;
        let conn = self.conn.clone();
        let conversation_id = conversation_id.to_owned();

        tokio::task::spawn_blocking(move || {
            let conn = lock_connection(&conn)?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages
                 WHERE conversation_id = ?1
                 ORDER BY sequence_number ASC
                 LIMIT ?2 OFFSET ?3"
            ))?;

            let rows = stmt.query_map(params![conversation_id, limit, offset], message_from_row)?;

            let mut messages = Vec::new();
            for msg in rows {
                messages.push(msg?);
            }
            Ok(messages)
        })
        .await
        .map_err(join_error)?
    }

    async fn list_conversations(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Conversation>, ConversationStoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = lock_connection(&conn)?;
            let mut stmt = conn.prepare(
                "SELECT id, title, parent_id, parent_message_id,
                        upstream_conversation_id, upstream_response_id, upstream_candidate_id,
                        created_at, updated_at
                 FROM conversations
                 ORDER BY updated_at DESC, id ASC
                 LIMIT ?1 OFFSET ?2",
            )?;

            let rows =
                stmt.query_map(params![limit as i64, offset as i64], conversation_from_row)?;

            let mut list = Vec::new();
            for conv in rows {
                list.push(conv?);
            }
            Ok(list)
        })
        .await
        .map_err(join_error)?
    }

    async fn delete_conversation(&self, id: &str) -> Result<(), ConversationStoreError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = lock_connection(&conn)?;
            let tx = conn.transaction()?;
            let affected = tx.execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
            if affected == 0 {
                return Err(ConversationStoreError::NotFound(id));
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(join_error)?
    }

    async fn persist_turn(&self, turn: Turn) -> Result<(), ConversationStoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = lock_connection(&conn)?;
            // Immediate obtains SQLite's write reservation before reading max
            // sequence, so another connection cannot allocate the same range.
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM conversations WHERE id = ?1",
                    params![turn.conversation_id],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if !exists {
                return Err(ConversationStoreError::NotFound(turn.conversation_id));
            }

            let last_sequence: i64 = tx.query_row(
                "SELECT COALESCE(MAX(sequence_number), 0) FROM messages WHERE conversation_id = ?1",
                params![turn.conversation_id],
                |row| row.get(0),
            )?;
            let mut next_sequence = last_sequence.checked_add(1).ok_or_else(|| {
                ConversationStoreError::Migration("message sequence exhausted".to_owned())
            })?;

            for mut message in turn.request_messages {
                message.conversation_id.clone_from(&turn.conversation_id);
                message.sequence_number = next_sequence;
                message.upstream_conversation_id = None;
                message.upstream_response_id = None;
                message.upstream_candidate_id = None;
                next_sequence = next_sequence.checked_add(1).ok_or_else(|| {
                    ConversationStoreError::Migration("message sequence exhausted".to_owned())
                })?;
                insert_message_tx(&tx, &message, false)?;
            }

            let mut response = turn.response;
            response.conversation_id = turn.conversation_id.clone();
            response.sequence_number = next_sequence;
            let has_upstream_ids = response.upstream_conversation_id.is_some()
                || response.upstream_response_id.is_some()
                || response.upstream_candidate_id.is_some();
            insert_message_tx(&tx, &response, has_upstream_ids)?;

            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(join_error)?
    }

    async fn branch_from(
        &self,
        source_conversation_id: &str,
        from_message_id: &str,
        title: Option<String>,
    ) -> Result<Conversation, ConversationStoreError> {
        let conn = self.conn.clone();
        let source_id = source_conversation_id.to_owned();
        let from_msg_id = from_message_id.to_owned();

        tokio::task::spawn_blocking(move || {
            let mut conn = lock_connection(&conn)?;
            crate::branch::execute_branch(&mut conn, &source_id, &from_msg_id, title)
        })
        .await
        .map_err(join_error)?
    }
}
fn join_error(error: tokio::task::JoinError) -> ConversationStoreError {
    ConversationStoreError::Migration(format!("task join error: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConversationStore;

    #[tokio::test]
    async fn poisoned_connection_surfaces_errors_instead_of_panicking() {
        let store = SqliteConversationStore::in_memory().unwrap();
        let conversation = store
            .create_conversation(None)
            .await
            .expect("create conversation");

        // Deliberately poison the mutex without allowing the panic to escape
        // the test task, then verify normal store operations return errors.
        let poisoned_store = store.clone();
        let handle = tokio::task::spawn_blocking(move || {
            let _guard = poisoned_store.conn.lock().unwrap();
            panic!("poison conversation store connection");
        });
        assert!(handle.await.is_err());

        assert!(matches!(
            store.get_conversation(&conversation.id).await,
            Err(ConversationStoreError::LockPoisoned(_))
        ));
        assert!(store.get_history(&conversation.id).await.is_err());
        assert!(
            store
                .append_message(StoredMessage {
                    id: Uuid::new_v4().to_string(),
                    conversation_id: conversation.id.clone(),
                    parent_message_id: None,
                    role: "user".to_owned(),
                    content_json: "\"hi\"".to_owned(),
                    sequence_number: 5,
                    created_at: now_unix(),
                    upstream_conversation_id: None,
                    upstream_response_id: None,
                    upstream_candidate_id: None,
                })
                .await
                .is_err()
        );
        assert!(
            store
                .update_upstream_ids(&conversation.id, None, None, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn concurrent_upstream_id_updates_publish_complete_tuples() {
        let store = SqliteConversationStore::in_memory().unwrap();
        let conversation = store.create_conversation(None).await.unwrap();
        let mut updates = Vec::new();

        for generation in 0..32 {
            let store = store.clone();
            let conversation_id = conversation.id.clone();
            updates.push(tokio::spawn(async move {
                let upstream_conversation = format!("conversation-{generation}");
                let upstream_response = format!("response-{generation}");
                let upstream_candidate = format!("candidate-{generation}");
                store
                    .update_upstream_ids(
                        &conversation_id,
                        Some(&upstream_conversation),
                        Some(&upstream_response),
                        Some(&upstream_candidate),
                    )
                    .await
            }));
        }

        for update in updates {
            update.await.unwrap().unwrap();
        }

        let persisted = store.get_conversation(&conversation.id).await.unwrap();
        let conversation_generation = persisted
            .upstream_conversation_id
            .as_deref()
            .unwrap()
            .strip_prefix("conversation-")
            .unwrap();
        let response_generation = persisted
            .upstream_response_id
            .as_deref()
            .unwrap()
            .strip_prefix("response-")
            .unwrap();
        let candidate_generation = persisted
            .upstream_candidate_id
            .as_deref()
            .unwrap()
            .strip_prefix("candidate-")
            .unwrap();
        assert_eq!(response_generation, conversation_generation);
        assert_eq!(candidate_generation, conversation_generation);
    }
}
