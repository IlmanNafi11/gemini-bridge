use crate::{
    MediaMetadata, MediaStore, MetadataFilter, MetadataPage, StoreError, cleanup,
    index::MetadataIndex,
};
use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::fs;
use uuid::Uuid;

/// Filesystem-backed content-addressed media store.
///
/// JSON sidecars are the source of truth. A durable SQLite index makes listing,
/// filtering, hash lookup, and reference accounting independent of repeated
/// sidecar scans. A filesystem lock serializes mutations across clones,
/// separately constructed handles, and processes sharing this data directory.
#[derive(Clone)]
pub struct LocalMediaStore {
    data_dir: PathBuf,
    default_ttl_days: u32,
    index: Arc<MetadataIndex>,
}

impl LocalMediaStore {
    /// Create a store rooted at `data_dir` with no default expiry.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        let media_dir = data_dir.join("media");
        Self {
            data_dir,
            default_ttl_days: 0,
            index: Arc::new(MetadataIndex::new(
                media_dir.join("index.sqlite"),
                media_dir.join("meta"),
            )),
        }
    }

    /// Apply this TTL to records whose `expires_at` is not explicitly set.
    /// A value of zero disables the default expiry.
    pub fn with_default_ttl_days(mut self, ttl_days: u32) -> Self {
        self.default_ttl_days = ttl_days;
        self
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

    async fn lock_mutations(&self) -> Result<File, StoreError> {
        let path = self.media_dir().join(".mutation.lock");
        let parent = path.parent().expect("mutation lock path has parent");
        ensure_private_dir(parent).await?;
        let file = tokio::task::spawn_blocking(move || open_lock_file(path))
            .await
            .map_err(|error| StoreError::Io(std::io::Error::other(error)))??;
        Ok(file)
    }

    async fn ensure_index(&self) -> Result<(), StoreError> {
        ensure_private_dir(&self.data_dir).await?;
        ensure_private_dir(&self.media_dir()).await?;
        ensure_private_dir(&self.metadata_dir()).await?;
        self.index.ensure_ready()
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

    async fn write_metadata_atomic(&self, metadata: &MediaMetadata) -> Result<(), StoreError> {
        let path = self
            .metadata_path(&metadata.id)
            .ok_or_else(|| StoreError::NotFound(metadata.id.clone()))?;
        let parent = path.parent().expect("metadata path always has parent");
        ensure_private_dir(parent).await?;

        let bytes = serde_json::to_vec(metadata)?;
        let tmp_path = parent.join(format!(".{}.{}.tmp", metadata.id, Uuid::new_v4()));
        write_private_file(&tmp_path, &bytes).await?;
        if let Err(error) = fs::rename(&tmp_path, &path).await {
            let _ = fs::remove_file(&tmp_path).await;
            return Err(error.into());
        }
        Ok(())
    }

    async fn remove_content_if_unreferenced(&self, sha256: &str) -> Result<(), StoreError> {
        if self.index.reference_count(sha256)? > 0 {
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

    async fn write_content_if_missing(
        &self,
        path: &Path,
        content: &[u8],
    ) -> Result<(), StoreError> {
        let parent = path.parent().expect("content path has parent");
        ensure_private_dir(parent).await?;
        match open_private_new(path).await {
            Ok(mut file) => {
                use tokio::io::AsyncWriteExt;
                if let Err(error) = file.write_all(content).await {
                    let _ = fs::remove_file(path).await;
                    return Err(error.into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl MediaStore for LocalMediaStore {
    async fn put(&self, content: Bytes, mut metadata: MediaMetadata) -> Result<String, StoreError> {
        let _guard = self.lock_mutations().await?;
        self.ensure_index().await?;

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
        if metadata.expires_at.is_none() {
            metadata.expires_at =
                cleanup::compute_default_expiry(metadata.created_at, self.default_ttl_days);
        }
        let metadata_path = self
            .metadata_path(&metadata.id)
            .ok_or_else(|| StoreError::NotFound(metadata.id.clone()))?;
        let previous = if fs::try_exists(&metadata_path).await? {
            Some(self.read_metadata(&metadata.id).await?)
        } else {
            None
        };

        self.index.begin_mutation()?;
        let content_path = self
            .content_path(&metadata.sha256)
            .expect("computed SHA-256 has valid format");
        self.write_content_if_missing(&content_path, &content)
            .await?;
        self.write_metadata_atomic(&metadata).await?;
        self.index.upsert(&metadata)?;
        if let Some(previous) = previous
            && previous.sha256 != metadata.sha256
        {
            self.remove_content_if_unreferenced(&previous.sha256)
                .await?;
        }
        self.index.finish_mutation()?;
        Ok(metadata.id)
    }

    async fn get(&self, id: &str) -> Result<(Bytes, MediaMetadata), StoreError> {
        let _guard = self.lock_mutations().await?;
        self.ensure_index().await?;
        let metadata = self.read_metadata(id).await?;
        let path = self
            .content_path(&metadata.sha256)
            .ok_or(StoreError::HashMismatch)?;
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
        let _guard = match self.lock_mutations().await {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        if self.ensure_index().await.is_err() {
            return false;
        }
        self.content_path(sha256).is_some_and(|path| path.is_file())
    }

    async fn find_by_hash(&self, sha256: &str) -> Option<String> {
        let _guard = self.lock_mutations().await.ok()?;
        self.ensure_index().await.ok()?;
        self.index.find_by_hash(sha256).ok().flatten()
    }

    async fn list(&self, limit: usize, offset: usize) -> Vec<MediaMetadata> {
        self.list_filtered(MetadataFilter::default(), limit, offset)
            .await
            .map(|page| page.items)
            .unwrap_or_default()
    }

    async fn list_filtered(
        &self,
        filter: MetadataFilter,
        limit: usize,
        offset: usize,
    ) -> Result<MetadataPage, StoreError> {
        let _guard = self.lock_mutations().await?;
        self.ensure_index().await?;
        self.index.list(&filter, limit, offset)
    }

    async fn delete(&self, id: &str) -> Result<(), StoreError> {
        let _guard = self.lock_mutations().await?;
        self.ensure_index().await?;
        let metadata = self.read_metadata(id).await?;
        let path = self
            .metadata_path(id)
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;

        self.index.begin_mutation()?;
        fs::remove_file(path).await?;
        self.index.remove(id)?;
        self.index.finish_mutation()?;
        self.remove_content_if_unreferenced(&metadata.sha256).await
    }

    async fn purge_expired(&self) -> Result<usize, StoreError> {
        let _guard = self.lock_mutations().await?;
        self.ensure_index().await?;
        let expired = self.index.expired(cleanup::now_unix())?;
        if expired.is_empty() {
            return Ok(0);
        }

        self.index.begin_mutation()?;
        for metadata in &expired {
            let path = self
                .metadata_path(&metadata.id)
                .ok_or_else(|| StoreError::NotFound(metadata.id.clone()))?;
            match fs::remove_file(path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            self.index.remove(&metadata.id)?;
        }
        self.index.finish_mutation()?;

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

async fn ensure_private_dir(path: &Path) -> Result<(), StoreError> {
    fs::create_dir_all(path).await?;
    set_directory_permissions(path).await
}

#[cfg(unix)]
async fn set_directory_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn set_directory_permissions(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

async fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let mut file = open_private_new(path).await?;
    use tokio::io::AsyncWriteExt;
    file.write_all(bytes).await?;
    Ok(())
}

#[cfg(unix)]
async fn open_private_new(path: &Path) -> Result<fs::File, std::io::Error> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .await
}

#[cfg(not(unix))]
async fn open_private_new(path: &Path) -> Result<fs::File, std::io::Error> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
}

fn open_lock_file(path: PathBuf) -> Result<File, StoreError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    set_file_permissions(&path)?;
    file.lock()?;
    Ok(file)
}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}
