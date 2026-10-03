use serde::{Deserialize, Serialize};

/// Represents a persistent conversation container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    /// Unique internal conversation identifier.
    pub id: String,
    /// Optional user-assigned or auto-generated title.
    pub title: Option<String>,
    /// Parent conversation identifier if this conversation was branched from another.
    pub parent_id: Option<String>,
    /// Message identifier in the parent conversation where the branch was anchored.
    pub parent_message_id: Option<String>,
    /// Upstream Gemini conversationId returned by Gemini Web backend.
    pub upstream_conversation_id: Option<String>,
    /// Upstream Gemini responseId from the latest turn.
    pub upstream_response_id: Option<String>,
    /// Upstream Gemini candidateId (e.g. "rc_...") from the latest turn.
    pub upstream_candidate_id: Option<String>,
    /// Unix timestamp (seconds) when the conversation was created.
    pub created_at: i64,
    /// Unix timestamp (seconds) when the conversation was last updated.
    pub updated_at: i64,
}

/// Represents a single stored message within a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMessage {
    /// Unique message identifier (UUID v4).
    pub id: String,
    /// Identifier of the conversation this message belongs to.
    pub conversation_id: String,
    /// Optional parent message identifier to preserve non-linear local graph ancestry.
    pub parent_message_id: Option<String>,
    /// Message role: "system", "user", "assistant", or "tool".
    pub role: String,
    /// JSON-serialized message content, supporting string or structured parts.
    pub content_json: String,
    /// Monotonically increasing sequence index within this conversation.
    pub sequence_number: i64,
    /// Unix timestamp (seconds) when the message was persisted.
    pub created_at: i64,
    /// Upstream Gemini identifiers after the turn that produced this message, when available.
    pub upstream_conversation_id: Option<String>,
    pub upstream_response_id: Option<String>,
    pub upstream_candidate_id: Option<String>,
}

/// All durable records produced by one successful upstream completion.
///
/// Sequence numbers on the supplied messages are ignored: the store allocates
/// one contiguous range while holding the SQLite write transaction. The
/// response carries the upstream identifiers returned for the completed turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// Conversation receiving the complete turn.
    pub conversation_id: String,
    /// Request messages sent to the provider, in wire order.
    pub request_messages: Vec<StoredMessage>,
    /// Assistant response and its upstream identifier snapshot.
    pub response: StoredMessage,
}
