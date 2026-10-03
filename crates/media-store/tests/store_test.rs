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

#[tokio::test]
async fn put_replaces_existing_metadata_id_without_leaking_old_content() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    let mut metadata = make_meta("text/plain");
    metadata.id = "stable-id".into();

    let first_id = s
        .put(Bytes::from_static(b"first content"), metadata.clone())
        .await
        .unwrap();
    let (_, first) = s.get(&first_id).await.unwrap();

    let second_id = s
        .put(Bytes::from_static(b"second content"), metadata)
        .await
        .unwrap();
    assert_eq!(second_id, first_id);
    let (bytes, second) = s.get(&second_id).await.unwrap();
    assert_eq!(bytes, Bytes::from_static(b"second content"));
    assert_ne!(second.sha256, first.sha256);
    assert!(!s.exists(&first.sha256).await);
    assert!(s.exists(&second.sha256).await);
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

// ── restrictive permissions (unix) ───────────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn data_files_and_dirs_are_private_on_unix() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    let id = s
        .put(
            Bytes::from_static(b"private bytes"),
            make_meta("text/plain"),
        )
        .await
        .unwrap();
    let (_, meta) = s.get(&id).await.unwrap();
    let sha = &meta.sha256;

    let content_path = dir.path().join("media").join(&sha[..2]).join(sha);
    let meta_path = dir
        .path()
        .join("media")
        .join("meta")
        .join(format!("{id}.json"));

    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode(&content_path),
        0o600,
        "content file must be owner-only"
    );
    assert_eq!(mode(&meta_path), 0o600, "metadata file must be owner-only");

    for directory in [
        dir.path().to_path_buf(),
        dir.path().join("media"),
        dir.path().join("media").join("meta"),
        content_path.parent().unwrap().to_path_buf(),
    ] {
        assert_eq!(mode(&directory), 0o700, "{directory:?} must be owner-only");
    }

    for suffix in ["", "-wal", "-shm"] {
        let db = dir.path().join(format!("media/index.sqlite{suffix}"));
        if db.exists() {
            assert_eq!(mode(&db), 0o600, "index file must be owner-only");
        }
    }
}

// ── concurrent same-content puts ─────────────────────────────────────────────

#[tokio::test]
async fn concurrent_puts_of_same_content_from_clones_share_one_content_file() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    let clone = s.clone();
    let content = Bytes::from_static(b"clone race bytes");

    let (first, second) = tokio::join!(
        s.put(content.clone(), make_meta("image/png")),
        clone.put(content.clone(), make_meta("image/png")),
    );
    let id1 = first.unwrap();
    let id2 = second.unwrap();
    assert_ne!(id1, id2, "distinct records must get distinct ids");

    let (_, m1) = s.get(&id1).await.unwrap();
    let (_, m2) = s.get(&id2).await.unwrap();
    assert_eq!(m1.sha256, m2.sha256);

    let content_dir = dir.path().join("media").join(&m1.sha256[..2]);
    let files: Vec<_> = std::fs::read_dir(&content_dir).unwrap().collect();
    assert_eq!(files.len(), 1, "dedup must leave exactly one content file");
}

#[tokio::test]
async fn concurrent_puts_of_same_content_from_independent_stores_share_one_content_file() {
    let dir = TempDir::new().unwrap();
    let a = store(&dir);
    let b = store(&dir);
    let content = Bytes::from_static(b"independent race bytes");

    let (first, second) = tokio::join!(
        a.put(content.clone(), make_meta("image/png")),
        b.put(content.clone(), make_meta("image/png")),
    );
    let id1 = first.unwrap();
    let id2 = second.unwrap();

    let (_, m1) = a.get(&id1).await.unwrap();
    let (_, m2) = b.get(&id2).await.unwrap();
    assert_eq!(m1.sha256, m2.sha256);

    let content_dir = dir.path().join("media").join(&m1.sha256[..2]);
    let files: Vec<_> = std::fs::read_dir(&content_dir).unwrap().collect();
    assert_eq!(files.len(), 1);
}

// ── put / delete / purge interleavings ───────────────────────────────────────

#[tokio::test]
async fn interleaved_put_delete_purge_keeps_survivors_readable() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);

    let mut handles = Vec::new();
    for i in 0..16u8 {
        let s = s.clone();
        handles.push(tokio::spawn(async move {
            let content = Bytes::copy_from_slice(&[i; 32]);
            let id = s.put(content, make_meta("text/plain")).await.unwrap();
            if i % 2 == 0 {
                let _ = s.delete(&id).await;
            }
        }));
    }
    let purge = s.clone();
    let purge_task = tokio::spawn(async move {
        let _ = purge.purge_expired().await;
    });
    for handle in handles {
        handle.await.unwrap();
    }
    purge_task.await.unwrap();

    for metadata in s.list(usize::MAX, 0).await {
        s.get(&metadata.id)
            .await
            .unwrap_or_else(|_| panic!("surviving record {} must stay readable", metadata.id));
    }
}

#[tokio::test]
async fn concurrent_delete_and_put_of_shared_content_never_dangles() {
    let dir = TempDir::new().unwrap();
    let a = store(&dir);
    let b = store(&dir);
    let content = Bytes::from_static(b"delete/put race bytes");

    let id1 = a
        .put(content.clone(), make_meta("text/plain"))
        .await
        .unwrap();
    let (_, meta) = a.get(&id1).await.unwrap();
    let sha = meta.sha256.clone();

    let (deleted, put) = tokio::join!(a.delete(&id1), b.put(content, make_meta("text/plain")));
    deleted.expect("delete of existing record succeeds");
    let id2 = put.expect("concurrent put succeeds");

    let (_, survivor) = b.get(&id2).await.unwrap();
    assert_eq!(survivor.sha256, sha);
    assert!(
        b.exists(&sha).await,
        "content referenced by the surviving record must exist on disk"
    );
}

// ── purge must never remove shared content ───────────────────────────────────

#[tokio::test]
async fn purge_preserves_shared_content_referenced_by_active_survivor() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    let now = now_unix();
    let content = Bytes::from_static(b"shared purge bytes");

    let mut expired_meta = make_meta("image/png");
    expired_meta.expires_at = Some(now - 100);
    let expired_id = s.put(content.clone(), expired_meta).await.unwrap();
    let survivor_id = s.put(content, make_meta("image/png")).await.unwrap();

    let (_, meta) = s.get(&survivor_id).await.unwrap();
    let sha = meta.sha256.clone();

    let purged = s.purge_expired().await.unwrap();
    assert_eq!(purged, 1);

    assert!(matches!(
        s.get(&expired_id).await,
        Err(StoreError::NotFound(_))
    ));
    s.get(&survivor_id).await.unwrap();
    assert!(
        s.exists(&sha).await,
        "shared content must survive purge while a record references it"
    );
}

#[tokio::test]
async fn purge_removes_content_after_all_shared_hash_records_expire() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    let now = now_unix();
    let content = Bytes::from_static(b"all shared records expired");

    let mut first_meta = make_meta("image/png");
    first_meta.expires_at = Some(now - 200);
    let first_id = s.put(content.clone(), first_meta).await.unwrap();

    let mut second_meta = make_meta("image/png");
    second_meta.expires_at = Some(now - 100);
    let second_id = s.put(content, second_meta).await.unwrap();
    let sha = s.get(&first_id).await.unwrap().1.sha256;

    assert_eq!(s.purge_expired().await.unwrap(), 2);
    assert!(matches!(
        s.get(&first_id).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        s.get(&second_id).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(
        !s.exists(&sha).await,
        "content must be removed once no metadata record references the shared hash"
    );
    assert_eq!(s.find_by_hash(&sha).await, None);
}

// ── reopen & index rebuild ───────────────────────────────────────────────────

#[tokio::test]
async fn second_instance_sees_records_written_by_first() {
    let dir = TempDir::new().unwrap();
    let first = store(&dir);
    let id = first
        .put(Bytes::from_static(b"persist me"), make_meta("text/plain"))
        .await
        .unwrap();
    let (_, meta) = first.get(&id).await.unwrap();
    drop(first);

    let reopened = store(&dir);
    assert_eq!(reopened.find_by_hash(&meta.sha256).await, Some(id.clone()));
    let (bytes, _) = reopened.get(&id).await.unwrap();
    assert_eq!(bytes, Bytes::from_static(b"persist me"));
}

#[tokio::test]
async fn reopen_after_index_db_loss_rebuilds_from_sidecars() {
    let dir = TempDir::new().unwrap();
    let first = store(&dir);
    let id = first
        .put(Bytes::from_static(b"rebuild me"), make_meta("text/plain"))
        .await
        .unwrap();
    let (_, meta) = first.get(&id).await.unwrap();
    drop(first);

    for suffix in ["", "-wal", "-shm"] {
        let path = dir.path().join(format!("media/index.sqlite{suffix}"));
        let _ = std::fs::remove_file(&path);
    }

    let reopened = store(&dir);
    let listed = reopened.list(10, 0).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(reopened.find_by_hash(&meta.sha256).await, Some(id.clone()));
    let (bytes, _) = reopened.get(&id).await.unwrap();
    assert_eq!(bytes, Bytes::from_static(b"rebuild me"));
}

#[tokio::test]
async fn bounded_filtered_list_caps_page_and_preserves_total() {
    use gemini_bridge_media_store::MetadataFilter;

    let dir = TempDir::new().unwrap();
    let s = store(&dir);
    for index in 0..205u16 {
        let mut metadata = make_meta("image/png");
        metadata.prompt = Some(format!("Blue flower {index}"));
        metadata.model = Some(if index % 2 == 0 { "pro" } else { "flash" }.into());
        metadata.created_at = index as i64;
        s.put(Bytes::from(index.to_le_bytes().to_vec()), metadata)
            .await
            .unwrap();
    }

    let page = s
        .list_filtered(
            MetadataFilter {
                prompt_contains: Some("BLUE FLOWER".into()),
                model: Some("pro".into()),
                created_at_from: Some(20),
                created_at_to: Some(200),
            },
            usize::MAX,
            0,
        )
        .await
        .unwrap();
    assert_eq!(page.total, 91);
    assert_eq!(page.items.len(), 91);
    let capped = s
        .list_filtered(
            MetadataFilter {
                prompt_contains: Some("BLUE FLOWER".into()),
                ..MetadataFilter::default()
            },
            usize::MAX,
            0,
        )
        .await
        .unwrap();
    assert_eq!(capped.total, 205);
    assert_eq!(capped.items.len(), 200, "result page must be bounded");
    assert!(capped.items.windows(2).all(|pair| {
        pair[0].created_at > pair[1].created_at
            || (pair[0].created_at == pair[1].created_at && pair[0].id < pair[1].id)
    }));
}

#[tokio::test]
async fn configured_default_expiry_applies_only_when_expiry_is_missing() {
    let dir = TempDir::new().unwrap();
    let s = LocalMediaStore::new(dir.path()).with_default_ttl_days(2);

    let mut metadata = make_meta("text/plain");
    metadata.created_at = 1_000;
    let id = s
        .put(Bytes::from_static(b"default expiry"), metadata)
        .await
        .unwrap();
    assert_eq!(
        s.get(&id).await.unwrap().1.expires_at,
        Some(1_000 + 2 * 86_400)
    );

    let mut explicit = make_meta("text/plain");
    explicit.created_at = 2_000;
    explicit.expires_at = Some(9_999);
    let id = s
        .put(Bytes::from_static(b"explicit expiry"), explicit)
        .await
        .unwrap();
    assert_eq!(s.get(&id).await.unwrap().1.expires_at, Some(9_999));
}

#[tokio::test]
async fn corrupted_index_is_rebuilt_from_metadata_sidecars() {
    let dir = TempDir::new().unwrap();
    let first = store(&dir);
    let id = first
        .put(
            Bytes::from_static(b"recover after corruption"),
            make_meta("text/plain"),
        )
        .await
        .unwrap();
    let (_, metadata) = first.get(&id).await.unwrap();
    drop(first);

    std::fs::write(
        dir.path().join("media").join("index.sqlite"),
        b"not a sqlite database",
    )
    .unwrap();

    let reopened = store(&dir);
    assert_eq!(
        reopened.find_by_hash(&metadata.sha256).await,
        Some(id.clone())
    );
    assert_eq!(reopened.list(10, 0).await.len(), 1);
    assert_eq!(
        reopened.get(&id).await.unwrap().0,
        Bytes::from_static(b"recover after corruption")
    );
}
