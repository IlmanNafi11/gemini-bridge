//! Transactional SQLite persistence for Gemini Bridge conversations.

mod branch;

mod db;
mod error;
mod migrations;
mod models;

pub use db::SqliteConversationStore;
pub use error::ConversationStoreError;
pub use models::{Conversation, StoredMessage};

/// Provider-neutral persistence operations for conversations and messages.
#[async_trait::async_trait]
pub trait ConversationStore: Send + Sync {
    async fn create_conversation(
        &self,
        title: Option<String>,
    ) -> Result<Conversation, ConversationStoreError>;

    async fn get_conversation(&self, id: &str) -> Result<Conversation, ConversationStoreError>;

    async fn update_upstream_ids(
        &self,
        id: &str,
        upstream_conversation_id: Option<&str>,
        upstream_response_id: Option<&str>,
        upstream_candidate_id: Option<&str>,
    ) -> Result<(), ConversationStoreError>;

    /// Atomically append a message and update its conversation's latest IDs.
    async fn append_message(&self, message: StoredMessage) -> Result<(), ConversationStoreError>;

    async fn get_history(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<StoredMessage>, ConversationStoreError>;

    async fn list_conversations(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Conversation>, ConversationStoreError>;

    async fn delete_conversation(&self, id: &str) -> Result<(), ConversationStoreError>;

    /// Create a branched conversation from `from_message_id`, copying ancestor
    /// history and seeding upstream IDs from that message's snapshot.
    async fn branch_from(
        &self,
        source_conversation_id: &str,
        from_message_id: &str,
        title: Option<String>,
    ) -> Result<Conversation, ConversationStoreError>;
}
