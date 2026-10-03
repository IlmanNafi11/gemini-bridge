# Module Specification: `conversation-store`

**Module ID:** `conversation-store`  
**Crate:** `gemini-bridge-conversation-store` (`crates/conversation-store`)  
**Phase:** Fase 2  
**Depends On:** `plugin-context`, `config`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-4, §4.5  
**Status:** Approved Draft — enriched for P.6  

---

## 1. Objective & Responsibility

The `conversation-store` module provides relational, transactional persistence for chat conversations, messages, ancestry branches, and upstream Gemini continuation identifiers (`conversationId`, `responseId`, `candidateId`) using embedded SQLite (`rusqlite`).

It enables client applications to maintain persistent multi-turn conversations, create conversation branches from arbitrary message points, regenerate responses from previous turns, and retrieve local conversation histories. When upstream Gemini rejects a continuation identifier, this module supplies the complete recorded message lineage to enable transparent client-side replay fallback with degraded continuity status headers (`x-gemini-bridge-continuity: degraded`).

**In scope:**
- Embedded single-file SQLite database management with deterministic schema migrations.
- Transactional persistence of `Conversation` metadata, including mapping to Gemini upstream IDs (`conversationId`, `responseId`, `candidateId`).
- On Unix, standalone database opens use a private (`0700`) parent only when creating a new data directory, while existing caller-owned parent modes remain unchanged; database and SQLite sidecar files use `0600`.
- Append-only message history persistence with ordered sequence index and timestamping.
- Branch creation (`POST /v1/conversations/{id}/branch`) yielding a new child conversation that inherits ancestor message history up to a specified message point.
- Querying paginated conversation lists and ordered message history for a given conversation.
- Reconstructing complete, ordered message history trees to support history replay fallback on upstream continuity failure.
- Optional local database path configuration and transaction rollback on storage errors.

**Out of scope:**
- HTTP routing and request parameter parsing (→ `http-server`).
- OpenAI wire format mapping and chat completion dispatch (→ `openai-compat`, `llm-service`).
- Upstream wire protocol serialization, `f.req` construction, or direct network transport (→ `gemini-adapter`, `transport`).
- Image or binary payload storage (→ `media-store`).
- Bearer authentication and request validation (→ `http-server`, `middleware`).

---

## 2. Public API & Interfaces

### 2.1 Domain Models

```rust
use serde::{Deserialize, Serialize};

/// Represents a persistent conversation container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    /// Unique internal conversation identifier (e.g. UUID v4 or alphanumeric slug).
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

/// Request parameters for creating a new conversation branch.
#[derive(Debug, Clone, Deserialize)]
pub struct BranchRequest {
    /// Target message ID in the source conversation to branch from.
    pub from_message_id: String,
    /// Optional custom title for the newly branched conversation.
    pub title: Option<String>,
}

/// Response returned when creating or retrieving a branch.
#[derive(Debug, Clone, Serialize)]
pub struct BranchResponse {
    /// The newly created branched conversation.
    pub conversation: Conversation,
    /// Number of ancestor messages copied or referenced into the branch lineage.
    pub inherited_message_count: usize,
}
```

### 2.2 Error Types

```rust
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
```

### 2.3 Store Trait and Service Interface

```rust
#[async_trait::async_trait]
pub trait ConversationStore: Send + Sync {
    /// Create a new empty conversation with optional title.
    async fn create_conversation(
        &self,
        title: Option<String>,
    ) -> Result<Conversation, ConversationStoreError>;

    /// Retrieve conversation metadata by internal ID.
    async fn get_conversation(
        &self,
        id: &str,
    ) -> Result<Conversation, ConversationStoreError>;

    /// Update upstream identifiers associated with a conversation after a successful turn.
    async fn update_upstream_ids(
        &self,
        id: &str,
        upstream_conversation_id: Option<&str>,
        upstream_response_id: Option<&str>,
        upstream_candidate_id: Option<&str>,
    ) -> Result<(), ConversationStoreError>;

    /// Append a message and its resulting upstream identifiers atomically.
    async fn append_message(
        &self,
        message: StoredMessage,
    ) -> Result<(), ConversationStoreError>;

    /// Retrieve complete ordered message history for a conversation (ascending by sequence).
    async fn get_history(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<StoredMessage>, ConversationStoreError>;

    /// Create a branched conversation from `from_message_id`, copying ancestor history
    /// and seeding continuation identifiers from that exact message's upstream snapshot.
    async fn branch_from(
        &self,
        source_conversation_id: &str,
        from_message_id: &str,
        title: Option<String>,
    ) -> Result<Conversation, ConversationStoreError>;

    /// List conversations with pagination (sorted descending by updated_at).
    async fn list_conversations(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Conversation>, ConversationStoreError>;

    /// Delete a conversation and all its messages.
    async fn delete_conversation(
        &self,
        id: &str,
    ) -> Result<(), ConversationStoreError>;
}
```

---

## 3. Database Schema & Migrations

SQLite table definitions:

```sql
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY,
    title TEXT,
    parent_id TEXT,
    parent_message_id TEXT,
    upstream_conversation_id TEXT,
    upstream_response_id TEXT,
    upstream_candidate_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY(parent_id) REFERENCES conversations(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    parent_message_id TEXT,
    role TEXT NOT NULL,
    content_json TEXT NOT NULL,
    sequence_number INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    upstream_conversation_id TEXT,
    upstream_response_id TEXT,
    upstream_candidate_id TEXT,
    FOREIGN KEY(conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_messages_conv_seq 
ON messages(conversation_id, sequence_number ASC);

CREATE INDEX IF NOT EXISTS idx_conversations_updated 
ON conversations(updated_at DESC);
```

---

## 4. Behavior & Invariants

1. **Transactional Atomicity:**
   Appending a message and its associated upstream identifiers, and updating the conversation's latest upstream identifiers, are committed in one SQLite transaction. If writing fails, all changes are rolled back.
2. **Append-Only Immutability:**
   Messages are immutable once written. Regeneration or editing creates either a new message at the end of the current conversation or a new conversation branch anchored at the target message point.
3. **Deterministic Branching Lineage:**
   When branching from `message_X` in `conv_A`, the new `conv_B` copies the exact sequence of ancestor messages up to and including `message_X`. `conv_B.parent_id` is set to `conv_A`, and `conv_B.parent_message_id` is set to `message_X`. Its upstream continuation identifiers are copied from the snapshot recorded on `message_X`, never from a later turn in `conv_A`.
4. **Degraded Continuity Fallback Support:**
   If upstream Gemini rejects continuation IDs (`conversationId` not recognized or session shifted), the chat integration can retrieve `get_history(conversation_id)`, reformat messages into a replay request, and set `x-gemini-bridge-continuity: degraded`. The store supplies ordered history and does not implement network fallback or response headers.
5. **Zero Credential Persistence:**
   Cookies, Google bearer tokens, auth hashes, or secret encryption keys are strictly prohibited from being stored in SQLite tables.
6. **SQL Safety:**
   All queries use parameterized statements (`?1`, `?2`). String concatenation to construct SQL is forbidden.

---
## 5. Acceptance Criteria

### US-4 Traceability (Persistent Conversations & Branching)

| US-4 Acceptance Criterion | Module Specification Coverage |
|---|---|
| Persist `conversationId` + `responseId` + `candidateId` mapped to internal `conversation_id` | `update_upstream_ids` and `conversations` table columns (`upstream_conversation_id`, `upstream_response_id`, `upstream_candidate_id`) |
| Request with `conversation_id` retrieves upstream IDs for continuation | `get_conversation` returns populated upstream identifiers |
| `POST /v1/conversations/{id}/branch` creates a branch from a specific message | `branch_from(source_conversation_id, from_message_id, title)` |
| `GET /v1/conversations` and `GET /v1/conversations/{id}/messages` return consistent history | `list_conversations` (paginated) and `get_history` (ordered by sequence) |
| Degraded continuity replay when upstream IDs rejected | `get_history` produces complete ordered message lineage for replay fallback |

---

## 6. Testing Strategy

1. **Schema Migration Tests:**
   - Fresh database initialization creates tables and indices cleanly.
   - Idempotent migration runs on already-migrated database without error.
2. **Transactional Integrity & Rollback:**
   - Inject error during message append and verify transaction rolls back without orphan records or sequence drift; the conversation's latest upstream IDs must remain unchanged as well.
3. **Multi-Turn Ordering & Persistence:**
   - Append a series of messages; close and reopen the SQLite connection; verify `get_history` returns messages in exact sequence order and each message's upstream ID snapshot round-trips.
4. **Branch Isolation Tests:**
   - Branch conversation at message 3 out of 5. Verify the branch has exactly messages 1..3 with continuous sequence numbers and receives continuation IDs from message 3, not the source conversation's later message 5.
   - Append message 4' to the branch; verify the original conversation remains unchanged with messages 1..5.
5. **Upstream ID Mapping Tests:**
   - Persist a successful turn's upstream identifiers; retrieve conversation and the associated message and verify both the latest mapping and message-level snapshot.
6. **Error Condition Tests:**
   - `get_conversation` on a non-existent ID returns `ConversationStoreError::NotFound`.
   - `branch_from` with a message ID that is missing or belongs to another conversation returns `ConversationStoreError::InvalidBranchPoint`.
7. **Filesystem Privacy (Unix):** Opening a database in an existing `0755` parent does not change the parent mode; database and existing SQLite sidecar files are secured to `0600`. A newly created parent is `0700`.

---
## 7. Boundaries

- **Always:** Use SQLite transactions for multi-row operations; use parameterized queries; enforce foreign key cascades; order history deterministically by `sequence_number ASC`.
- **Ask First:** Changing schema migrations or altering conversation branching semantics.
- **Never:** Store Google session cookies, authentication tokens, or credentials in SQLite; construct SQL via format strings or concatenation; drop tables during migration.
