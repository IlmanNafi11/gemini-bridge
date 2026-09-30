# Module Specification: `conversation-store`

**Module ID:** `conversation-store`  
**Crate:** `gemini-bridge-conversation-store` (`crates/conversation-store`)  
**Phase:** Fase 2  
**Depends On:** `plugin-context`, `config`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-4, §4.5  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Persist conversation records and ordered messages in local SQLite, including internal `conversation_id` mappings to Gemini `conversationId`, `responseId`, and `candidateId`. Support history queries, branch ancestry, regeneration from prior state, and durable replay input when Gemini rejects an upstream continuation identifier.

---

## 2. Public API & Interfaces

```rust
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConversationStoreError {
    #[error("Conversation not found")]
    NotFound,
    #[error("Storage operation failed")]
    Database(#[from] rusqlite::Error),
    #[error("Invalid branch point")]
    InvalidBranchPoint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: Option<String>,
    pub parent_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub upstream_conversation_id: Option<String>,
    pub upstream_response_id: Option<String>,
    pub upstream_candidate_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub conversation_id: String,
    pub parent_message_id: Option<String>,
    pub role: String,
    pub content_json: String,
    pub created_at: i64,
}

#[async_trait::async_trait]
pub trait ConversationStore: Send + Sync {
    async fn create_conversation(&self) -> Result<Conversation, ConversationStoreError>;
    async fn append_message(&self, message: StoredMessage) -> Result<(), ConversationStoreError>;
    async fn get_history(&self, id: &str) -> Result<Vec<StoredMessage>, ConversationStoreError>;
    async fn branch_from(&self, id: &str, message_id: &str) -> Result<Conversation, ConversationStoreError>;
    async fn list(&self, limit: usize, offset: usize) -> Result<Vec<Conversation>, ConversationStoreError>;
}
```

---

## 3. Behavior & Invariants

1. **Atomicity:** A message append and associated upstream IDs are committed in one SQLite transaction.
2. **Append-Only History:** Existing messages are not edited in place; edit/regenerate creates a new branch or new message lineage.
3. **Branching:** Branch records identify a parent conversation and message point; branch history includes only the selected ancestor prefix plus branch-local messages.
4. **Continuity Fallback:** If Gemini rejects upstream IDs, service returns stored replay history to caller and marks continuity degraded at API boundary; store does not decide HTTP headers.
5. **Sensitive Data:** Messages and images may contain user data; storage paths are local, and purge/deletion behavior is explicit. Credentials are never stored here.

---

## 4. Testing Strategy

- SQLite migration and transaction rollback tests using temporary database files.
- Multi-turn persistence/order tests across close/reopen.
- Branch-from-middle test verifies ancestor prefix and branch isolation.
- Upstream ID mapping update/retrieval tests.
- Corrupt/missing DB and schema migration failures return structured errors without data loss.

---

## 5. Boundaries

- **Always:** Use transactions for multi-record state changes and parameterized SQL; preserve deterministic message order.
- **Ask First:** Schema migrations that delete/rewrite history, encryption format, or database path changes.
- **Never:** Persist Google cookies/tokens in conversation tables; execute SQL constructed from user input.
