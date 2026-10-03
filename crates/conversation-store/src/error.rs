use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConversationStoreError {
    #[error("Conversation not found: {0}")]
    NotFound(String),
    #[error("Message sequence numbers must be positive, got {0}")]
    InvalidSequence(i64),
    #[error("Pagination {0} is too large for SQLite: {1}")]
    PaginationOverflow(&'static str, usize),

    #[error("Sequence number {0} already exists in conversation {1}")]
    SequenceConflict(i64, String),

    #[error("Message already exists in conversation {0}: {1}")]
    DuplicateMessage(String, String),

    #[error("Store lock poisoned by a previous panic: {0}")]
    LockPoisoned(String),

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
