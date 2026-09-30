use crate::{MediaMetadata, MediaStore, StoreError, cleanup};
use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{fs, sync::Mutex};
use uuid::Uuid;

/// Filesystem-backed content-addressed media store.
///
/// Data layout:
/// ```text
/// {data_dir}/media/{sha256[0..2]}/{sha256}  # content bytes
/// {data_dir}/media/meta/{id}.json           # metadata record
/// ```
///
/// A small async mutex serializes mutations performed through this store
/// instance so that content and metadata operations remain consistent.
#[derive(Clone)]
pub struct LocalMediaStore {
    data_dir: PathBuf,
    mutation_lock: Arc<Mutex<()>>,
}

impl LocalMediaStore {
    /// Create a store rooted at `data_dir`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            mutation_lock: Arc::new(Mutex::new(())),
        }
    }

    fn media_dir(&self) -> PathBuf {
        self.data_dir.join("media")
    }

    fn metadata_dir(&self) -> PathBuf {
        self.media_dir().join("meta")
    }

    fn content_path(&self, sha256: &str) -> Option<PathBuf> {
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        Some(self.media_dir().join(&sha256[..2]).join(sha256))
    }

    fn metadata_path(&self, id: &str) -> Option<PathBuf> {
        // IDs are filenames: reject path traversal and absolute paths.
        if id.is_empty()
            || id == "."
            || id == ".."
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return None;
        }
        Some(self.metadata_dir().join(format!("{id}.json")))
    }

    async fn read_metadata(&self, id: &str) -> Result<MediaMetadata, StoreError> {
        let path = self
            .metadata_path(id)
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        let bytes = match fs::read(path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound(id.to_owned()));
            }
            Err(error) => return Err(error.into()),
        };
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn metadata_records(&self) -> Result<Vec<MediaMetadata>, StoreError> {
        let mut entries = match fs::read_dir(self.metadata_dir()).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };

        let mut records = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_file()
                || entry.path().extension().and_then(|ext| ext.to_str()) != Some("json")
            {
                continue;
            }
            let bytes = fs::read(entry.path()).await?;
            records.push(serde_json::from_slice(&bytes)?);
        }
        Ok(records)
    }

    async fn write_metadata_atomic(&self, metadata: &MediaMetadata) -> Result<(), StoreError> {
        let path = self
            .metadata_path(&metadata.id)
            .ok_or_else(|| StoreError::NotFound(metadata.id.clone()))?;
        let parent = path.parent().expect("metadata path always has parent");
        fs::create_dir_all(parent).await?;

        let bytes = serde_json::to_vec(metadata)?;
        let tmp_path = parent.join(format!(".{}.{}.tmp", metadata.id, Uuid::new_v4()));
        fs::write(&tmp_path, bytes).await?;
        if let Err(error) = fs::rename(&tmp_path, &path).await {
            let _ = fs::remove_file(&tmp_path).await;
            return Err(error.into());
        }
        Ok(())
    }

    async fn remove_content_if_unreferenced(&self, sha256: &str) -> Result<(), StoreError> {
        if self
            .metadata_records()
            .await?
            .iter()
            .any(|meta| meta.sha256 == sha256)
        {
            return Ok(());
        }
        if let Some(path) = self.content_path(sha256) {
            match fs::remove_file(path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl MediaStore for LocalMediaStore {
    async fn put(&self, content: Bytes, mut metadata: MediaMetadata) -> Result<String, StoreError> {
        let _guard = self.mutation_lock.lock().await;

        let actual_hash = sha256_hex(&content);
        if !metadata.sha256.is_empty() && metadata.sha256 != actual_hash {
            return Err(StoreError::HashMismatch);
        }
        metadata.sha256 = actual_hash;
        metadata.size_bytes = content.len() as u64;
        if metadata.id.is_empty() {
            metadata.id = Uuid::new_v4().to_string();
        }
        if metadata.created_at == 0 {
            metadata.created_at = cleanup::now_unix();
        }

        let content_path = self
            .content_path(&metadata.sha256)
            .expect("computed SHA-256 has valid format");
        let content_dir = content_path.parent().expect("content path has parent");
        fs::create_dir_all(content_dir).await?;

        // `create_new` ensures an existing content object is never overwritten,
        // including when multiple store instances race to write the same hash.
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&content_path)
            .await
        {
            Ok(mut file) => {
                use tokio::io::AsyncWriteExt;
                if let Err(error) = file.write_all(&content).await {
                    let _ = fs::remove_file(&content_path).await;
                    return Err(error.into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }

        self.write_metadata_atomic(&metadata).await?;
        Ok(metadata.id)
    }

    async fn get(&self, id: &str) -> Result<(Bytes, MediaMetadata), StoreError> {
        let metadata = self.read_metadata(id).await?;
        let path = self
            .content_path(&metadata.sha256)
            .ok_or_else(|| StoreError::HashMismatch)?;
        let content = match fs::read(path).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound(format!(
                    "content for sha256 {}",
                    metadata.sha256
                )));
            }
            Err(error) => return Err(error.into()),
        };
        Ok((Bytes::from(content), metadata))
    }

    async fn exists(&self, sha256: &str) -> bool {
        self.content_path(sha256).is_some_and(|path| path.is_file())
    }

    async fn find_by_hash(&self, sha256: &str) -> Option<String> {
        let mut records = self.metadata_records().await.ok()?;
        records.retain(|metadata| metadata.sha256 == sha256);
        records.sort_by(|left, right| left.id.cmp(&right.id));
        records.into_iter().next().map(|metadata| metadata.id)
    }

    async fn list(&self, limit: usize, offset: usize) -> Vec<MediaMetadata> {
        if limit == 0 {
            return Vec::new();
        }
        let Ok(mut records) = self.metadata_records().await else {
            return Vec::new();
        };
        records.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        records.into_iter().skip(offset).take(limit).collect()
    }

    async fn delete(&self, id: &str) -> Result<(), StoreError> {
        let _guard = self.mutation_lock.lock().await;
        let metadata = self.read_metadata(id).await?;
        let path = self
            .metadata_path(id)
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        fs::remove_file(path).await?;
        self.remove_content_if_unreferenced(&metadata.sha256).await
    }

    async fn purge_expired(&self) -> Result<usize, StoreError> {
        let _guard = self.mutation_lock.lock().await;
        let now = cleanup::now_unix();
        let expired: Vec<MediaMetadata> = self
            .metadata_records()
            .await?
            .into_iter()
            .filter(|metadata| cleanup::is_expired(metadata, now))
            .collect();

        for metadata in &expired {
            let path = self
                .metadata_path(&metadata.id)
                .ok_or_else(|| StoreError::NotFound(metadata.id.clone()))?;
            match fs::remove_file(path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }

        let mut removed_hashes = expired
            .iter()
            .map(|metadata| metadata.sha256.clone())
            .collect::<Vec<_>>();
        removed_hashes.sort();
        removed_hashes.dedup();
        for hash in removed_hashes {
            self.remove_content_if_unreferenced(&hash).await?;
        }

        Ok(expired.len())
    }
}

fn sha256_hex(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[allow(dead_code)]
fn _path_is_within(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}
