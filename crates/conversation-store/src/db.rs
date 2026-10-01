use crate::error::ConversationStoreError;
use crate::models::{Conversation, StoredMessage};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Embedded SQLite implementation of `ConversationStore`.
///
/// Wraps a single SQLite connection protected by a `std::sync::Mutex` so it is
/// safely shared across async tasks via `tokio::task::spawn_blocking`.
#[derive(Clone)]
pub struct SqliteConversationStore {
    conn: Arc<Mutex<Connection>>,
    #[allow(dead_code)]
    db_path: Option<PathBuf>,
}

impl SqliteConversationStore {
    /// Open or create a conversation store at the specified filesystem path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ConversationStoreError> {
        let path_ref = path.as_ref();
        if let Some(parent) = path_ref.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ConversationStoreError::Migration(format!("failed to create db dir: {e}"))
            })?;
        }
        let mut conn = Connection::open(path_ref)?;
        crate::migrations::apply_migrations(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: Some(path_ref.to_path_buf()),
        })
    }

    /// Open an in-memory conversation store (useful for tests).
    pub fn in_memory() -> Result<Self, ConversationStoreError> {
        let mut conn = Connection::open_in_memory()?;
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

#[async_trait::async_trait]
impl crate::ConversationStore for SqliteConversationStore {
    async fn create_conversation(
        &self,
        title: Option<String>,
    ) -> Result<Conversation, ConversationStoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
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
        .map_err(|e| ConversationStoreError::Migration(format!("task join error: {e}")))?
    }

    async fn get_conversation(&self, id: &str) -> Result<Conversation, ConversationStoreError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT id, title, parent_id, parent_message_id, upstream_conversation_id, upstream_response_id, upstream_candidate_id, created_at, updated_at
                 FROM conversations WHERE id = ?1",
            )?;

            let conv = stmt
                .query_row(params![id], |row| {
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
                })
                .optional()?;

            conv.ok_or_else(|| ConversationStoreError::NotFound(id))
        })
        .await
        .map_err(|e| ConversationStoreError::Migration(format!("task join error: {e}")))?
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
            let conn = conn.lock().unwrap();
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
        .map_err(|e| ConversationStoreError::Migration(format!("task join error: {e}")))?
    }

    async fn append_message(&self, message: StoredMessage) -> Result<(), ConversationStoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().unwrap();
            let tx = conn.transaction()?;

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

            // Insert message.
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
            )?;

            // Update conversation's updated_at and optionally upstream IDs.
            let now = now_unix();
            if message.upstream_conversation_id.is_some()
                || message.upstream_response_id.is_some()
                || message.upstream_candidate_id.is_some()
            {
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

            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|e| ConversationStoreError::Migration(format!("task join error: {e}")))?
    }

    async fn get_history(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<StoredMessage>, ConversationStoreError> {
        let conn = self.conn.clone();
        let conversation_id = conversation_id.to_owned();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT id, conversation_id, parent_message_id, role, content_json,
                        sequence_number, created_at, upstream_conversation_id,
                        upstream_response_id, upstream_candidate_id
                 FROM messages
                 WHERE conversation_id = ?1
                 ORDER BY sequence_number ASC",
            )?;

            let rows = stmt.query_map(params![conversation_id], |row| {
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
            })?;

            let mut messages = Vec::new();
            for msg in rows {
                messages.push(msg?);
            }
            Ok(messages)
        })
        .await
        .map_err(|e| ConversationStoreError::Migration(format!("task join error: {e}")))?
    }

    async fn list_conversations(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Conversation>, ConversationStoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT id, title, parent_id, parent_message_id,
                        upstream_conversation_id, upstream_response_id, upstream_candidate_id,
                        created_at, updated_at
                 FROM conversations
                 ORDER BY updated_at DESC, id ASC
                 LIMIT ?1 OFFSET ?2",
            )?;

            let rows = stmt.query_map(params![limit as i64, offset as i64], |row| {
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
            })?;

            let mut list = Vec::new();
            for conv in rows {
                list.push(conv?);
            }
            Ok(list)
        })
        .await
        .map_err(|e| ConversationStoreError::Migration(format!("task join error: {e}")))?
    }

    async fn delete_conversation(&self, id: &str) -> Result<(), ConversationStoreError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let affected = conn.execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
            if affected == 0 {
                return Err(ConversationStoreError::NotFound(id));
            }
            Ok(())
        })
        .await
        .map_err(|e| ConversationStoreError::Migration(format!("task join error: {e}")))?
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
            let mut conn = conn.lock().unwrap();
            crate::branch::execute_branch(&mut conn, &source_id, &from_msg_id, title)
        })
        .await
        .map_err(|e| ConversationStoreError::Migration(format!("task join error: {e}")))?
    }
}
