use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConversationStoreError {
    #[error("Conversation not found: {0}")]
    NotFound(String),

    #[error("Message not found: {0}")]
    MessageNotFound(String),

    #[error("Invalid branch point: message {0} does not belong to conversation {1}")]
    InvalidBranchPoint(String, String),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("Database migration error: {0}")]
    Migration(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
