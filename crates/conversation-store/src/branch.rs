//! Branching logic for `conversation-store`.

use crate::error::ConversationStoreError;
use crate::models::{Conversation, StoredMessage};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Execute branch creation from `from_message_id` inside an exclusive transaction.
pub fn execute_branch(
    conn: &mut Connection,
    source_conversation_id: &str,
    from_message_id: &str,
    title: Option<String>,
) -> Result<Conversation, ConversationStoreError> {
    let tx = conn.transaction()?;

    // 1. Verify source conversation exists.
    let (source_id, source_title): (String, Option<String>) = tx
        .query_row(
            "SELECT id, title FROM conversations WHERE id = ?1",
            params![source_conversation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| ConversationStoreError::NotFound(source_conversation_id.to_owned()))?;

    // 2. Fetch all messages in the source conversation ordered by sequence.
    let source_messages: Vec<StoredMessage> = {
        let mut stmt = tx.prepare(
            "SELECT id, conversation_id, parent_message_id, role, content_json,
                    sequence_number, created_at, upstream_conversation_id,
                    upstream_response_id, upstream_candidate_id
             FROM messages
             WHERE conversation_id = ?1
             ORDER BY sequence_number ASC",
        )?;

        let message_rows = stmt.query_map(params![source_id], |row| {
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
        for row in message_rows {
            messages.push(row?);
        }
        messages
    };

    // 3. Locate target message and isolate ancestor lineage.
    let target_idx = source_messages
        .iter()
        .position(|m| m.id == from_message_id)
        .ok_or_else(|| {
            ConversationStoreError::InvalidBranchPoint(
                from_message_id.to_owned(),
                source_conversation_id.to_owned(),
            )
        })?;

    let target_msg = &source_messages[target_idx];
    let branch_title = title.or_else(|| source_title.as_deref().map(|t| format!("{t} (branch)")));

    let new_conv_id = Uuid::new_v4().to_string();
    let now = now_unix();

    let new_conversation = Conversation {
        id: new_conv_id.clone(),
        title: branch_title.clone(),
        parent_id: Some(source_conversation_id.to_owned()),
        parent_message_id: Some(from_message_id.to_owned()),
        upstream_conversation_id: target_msg.upstream_conversation_id.clone(),
        upstream_response_id: target_msg.upstream_response_id.clone(),
        upstream_candidate_id: target_msg.upstream_candidate_id.clone(),
        created_at: now,
        updated_at: now,
    };

    tx.execute(
        "INSERT INTO conversations (
            id, title, parent_id, parent_message_id,
            upstream_conversation_id, upstream_response_id, upstream_candidate_id,
            created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            new_conversation.id,
            new_conversation.title,
            new_conversation.parent_id,
            new_conversation.parent_message_id,
            new_conversation.upstream_conversation_id,
            new_conversation.upstream_response_id,
            new_conversation.upstream_candidate_id,
            new_conversation.created_at,
            new_conversation.updated_at,
        ],
    )?;

    // 4. Copy ancestor messages up to target_idx into the new conversation.
    for ancestor in &source_messages[..=target_idx] {
        let msg_id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO messages (
                id, conversation_id, parent_message_id, role, content_json,
                sequence_number, created_at, upstream_conversation_id,
                upstream_response_id, upstream_candidate_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                msg_id,
                new_conv_id,
                ancestor.parent_message_id,
                ancestor.role,
                ancestor.content_json,
                ancestor.sequence_number,
                ancestor.created_at,
                ancestor.upstream_conversation_id,
                ancestor.upstream_response_id,
                ancestor.upstream_candidate_id,
            ],
        )?;
    }

    tx.commit()?;
    Ok(new_conversation)
}
