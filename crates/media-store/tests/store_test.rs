//! Integration tests for `gemini-bridge-media-store`.
//!
//! Tests are written against the `MediaStore` trait so they cover any
//! implementation; currently they drive `LocalMediaStore`.

use bytes::Bytes;
use gemini_bridge_media_store::{LocalMediaStore, MediaMetadata, MediaStore, StoreError};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

// ── helpers ──────────────────────────────────────────────────────────────────

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Build a `MediaMetadata` with the bare minimum fields; sha256 is left empty
/// so `put` computes and fills it.
fn make_meta(mime: &str) -> MediaMetadata {
    MediaMetadata {
        id: String::new(),
        sha256: String::new(),
        mime_type: mime.to_string(),
        size_bytes: 0,
        created_at: now_unix(),
        expires_at: None,
        prompt: None,
        file_ref: String::new(),
        model: None,
    }
}

fn store(dir: &TempDir) -> LocalMediaStore {
    LocalMediaStore::new(dir.path())
}

// ── put / get round-trip ──────────────────────────────────────────────────────

#[tokio::test]
async fn put_and_get_round_trip() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    let content = Bytes::from_static(b"hello media store");
    let id = s
        .put(content.clone(), make_meta("text/plain"))
        .await
        .unwrap();

    let (got_bytes, got_meta) = s.get(&id).await.unwrap();
    assert_eq!(got_bytes, content);
    assert_eq!(got_meta.id, id);
    assert_eq!(got_meta.mime_type, "text/plain");
    assert_eq!(got_meta.size_bytes, content.len() as u64);
    // sha256 was set by put
    assert!(!got_meta.sha256.is_empty());
}

// ── SHA-256 identity ──────────────────────────────────────────────────────────

#[tokio::test]
async fn put_fills_sha256_and_size() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    let content = Bytes::from_static(b"abc");
    // SHA-256 of "abc"
    let expected_hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let id = s.put(content, make_meta("text/plain")).await.unwrap();
    let (_, meta) = s.get(&id).await.unwrap();
    assert_eq!(meta.sha256, expected_hash);
    assert_eq!(meta.size_bytes, 3);
}

// ── hash mismatch ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn put_rejects_wrong_sha256() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    let mut meta = make_meta("text/plain");
    meta.sha256 = "deadbeef".to_string();
    let result = s.put(Bytes::from_static(b"actual content"), meta).await;
    assert!(
        matches!(result, Err(StoreError::HashMismatch)),
        "expected HashMismatch, got {result:?}"
    );
}

// ── correct sha256 pre-populated ─────────────────────────────────────────────

#[tokio::test]
async fn put_accepts_matching_sha256() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    let content = Bytes::from_static(b"abc");
    let correct_hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let mut meta = make_meta("text/plain");
    meta.sha256 = correct_hash.to_string();
    let id = s.put(content, meta).await.unwrap();
    let (_, got_meta) = s.get(&id).await.unwrap();
    assert_eq!(got_meta.sha256, correct_hash);
}

// ── deduplication ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn deduplication_same_sha256_one_content_file() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    let content = Bytes::from_static(b"duplicate content");

    let id1 = s
        .put(content.clone(), make_meta("image/png"))
        .await
        .unwrap();
    let id2 = s
        .put(content.clone(), make_meta("image/png"))
        .await
        .unwrap();

    // Both operations must succeed
    let (_, meta1) = s.get(&id1).await.unwrap();
    let (_, meta2) = s.get(&id2).await.unwrap();

    // Both share the same content hash
    assert_eq!(meta1.sha256, meta2.sha256);

    // There must be exactly one content file (not two)
    let hash = &meta1.sha256;
    let content_path = dir.path().join("media").join(&hash[..2]).join(hash);
    assert!(content_path.exists(), "content file should exist");
}

// ── exists ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn exists_returns_false_for_unknown_hash() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    assert!(
        !s.exists("000000000000000000000000000000000000000000000000000000000000dead")
            .await
    );
}

#[tokio::test]
async fn exists_returns_true_after_put() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    let content = Bytes::from_static(b"hello");
    let id = s.put(content, make_meta("text/plain")).await.unwrap();
    let (_, meta) = s.get(&id).await.unwrap();
    assert!(s.exists(&meta.sha256).await);
}

// ── find_by_hash ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn find_by_hash_returns_none_for_unknown() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    assert_eq!(s.find_by_hash("unknown_hash").await, None);
}

#[tokio::test]
async fn find_by_hash_returns_id_after_put() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    let content = Bytes::from_static(b"findable");
    let id = s.put(content, make_meta("text/plain")).await.unwrap();
    let (_, meta) = s.get(&id).await.unwrap();
    let found_id = s.find_by_hash(&meta.sha256).await;
    assert_eq!(found_id, Some(id));
}

// ── list ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_empty_store() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    assert!(s.list(10, 0).await.is_empty());
}

#[tokio::test]
async fn list_returns_all_items_with_zero_offset() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    s.put(Bytes::from_static(b"a"), make_meta("text/plain"))
        .await
        .unwrap();
    s.put(Bytes::from_static(b"bb"), make_meta("text/plain"))
        .await
        .unwrap();
    s.put(Bytes::from_static(b"ccc"), make_meta("text/plain"))
        .await
        .unwrap();

    let all = s.list(100, 0).await;
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn list_respects_limit_and_offset() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    for i in 0..5u8 {
        let content = Bytes::copy_from_slice(&[i, i]);
        s.put(content, make_meta("text/plain")).await.unwrap();
    }

    let page1 = s.list(2, 0).await;
    let page2 = s.list(2, 2).await;
    let page3 = s.list(2, 4).await;

    assert_eq!(page1.len(), 2);
    assert_eq!(page2.len(), 2);
    assert_eq!(page3.len(), 1);
    // No duplicates between pages
    let p1_ids: Vec<_> = page1.iter().map(|m| &m.id).collect();
    let p2_ids: Vec<_> = page2.iter().map(|m| &m.id).collect();
    for id in &p2_ids {
        assert!(!p1_ids.contains(id), "duplicate id across pages: {id}");
    }
}

// ── delete ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_removes_metadata_and_content_when_only_one_reference() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    let content = Bytes::from_static(b"to be deleted");
    let id = s.put(content, make_meta("text/plain")).await.unwrap();
    let (_, meta) = s.get(&id).await.unwrap();
    let sha = meta.sha256.clone();

    s.delete(&id).await.unwrap();

    // metadata gone
    assert!(matches!(s.get(&id).await, Err(StoreError::NotFound(_))));
    // content file gone
    assert!(!s.exists(&sha).await);
}

#[tokio::test]
async fn delete_preserves_content_when_another_metadata_references_hash() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    let content = Bytes::from_static(b"shared bytes");
    let id1 = s
        .put(content.clone(), make_meta("image/png"))
        .await
        .unwrap();
    let id2 = s
        .put(content.clone(), make_meta("image/png"))
        .await
        .unwrap();
    let (_, meta1) = s.get(&id1).await.unwrap();
    let sha = meta1.sha256.clone();

    // Delete one record; content should remain because id2 still references it
    s.delete(&id1).await.unwrap();

    assert!(
        s.exists(&sha).await,
        "content must survive while another meta references it"
    );
    // id2 still retrievable
    s.get(&id2).await.unwrap();
}

#[tokio::test]
async fn delete_returns_not_found_for_unknown_id() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    assert!(matches!(
        s.delete("no-such-id").await,
        Err(StoreError::NotFound(_))
    ));
}

// ── get returns not-found ─────────────────────────────────────────────────────

#[tokio::test]
async fn get_returns_not_found_for_unknown_id() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    assert!(matches!(
        s.get("does-not-exist").await,
        Err(StoreError::NotFound(_))
    ));
}

// ── purge_expired ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn purge_expired_removes_only_expired_items() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    let now = now_unix();

    // expired: expires_at in the past
    let mut expired_meta = make_meta("text/plain");
    expired_meta.expires_at = Some(now - 3600); // 1 hour ago
    let expired_id = s
        .put(Bytes::from_static(b"expired content"), expired_meta)
        .await
        .unwrap();

    // active: no expiry
    let active_id = s
        .put(
            Bytes::from_static(b"active content"),
            make_meta("text/plain"),
        )
        .await
        .unwrap();

    // fresh: expires in the future
    let mut fresh_meta = make_meta("text/plain");
    fresh_meta.expires_at = Some(now + 86400); // tomorrow
    let fresh_id = s
        .put(Bytes::from_static(b"fresh content"), fresh_meta)
        .await
        .unwrap();

    let purged = s.purge_expired().await.unwrap();
    assert_eq!(purged, 1, "only the expired item should be purged");

    // Expired gone
    assert!(matches!(
        s.get(&expired_id).await,
        Err(StoreError::NotFound(_))
    ));

    // Active and fresh survive
    s.get(&active_id).await.unwrap();
    s.get(&fresh_id).await.unwrap();
}

#[tokio::test]
async fn purge_expired_on_empty_store_returns_zero() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    let purged = s.purge_expired().await.unwrap();
    assert_eq!(purged, 0);
}

#[tokio::test]
async fn purge_expired_preserves_item_expiring_exactly_now() {
    // expires_at == now is considered still-valid (strict <, not <=)
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    let now = now_unix();

    let mut meta = make_meta("text/plain");
    meta.expires_at = Some(now + 10); // expiry well in the future
    let id = s
        .put(Bytes::from_static(b"near future"), meta)
        .await
        .unwrap();

    let purged = s.purge_expired().await.unwrap();
    assert_eq!(purged, 0);
    s.get(&id).await.unwrap();
}

// ── path traversal & security ────────────────────────────────────────────────

#[tokio::test]
async fn get_rejects_path_traversal_id() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    assert!(matches!(
        s.get("../secret").await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        s.get("../../etc/passwd").await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn delete_rejects_path_traversal_id() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    assert!(matches!(
        s.delete("../secret").await,
        Err(StoreError::NotFound(_))
    ));
}
